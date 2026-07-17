package reproittui

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
)

type causalExchange struct {
	ID              string            `json:"id"`
	Actor           string            `json:"actor"`
	ActionIndex     int               `json:"actionIndex"`
	Ordinal         int               `json:"ordinal"`
	Protocol        string            `json:"protocol"`
	Method          string            `json:"method"`
	URL             string            `json:"url"`
	RequestHeaders  map[string]string `json:"requestHeaders"`
	RequestBody     interface{}       `json:"requestBody,omitempty"`
	Status          int               `json:"status"`
	ResponseHeaders map[string]string `json:"responseHeaders"`
	ResponseBody    interface{}       `json:"responseBody,omitempty"`
	Required        bool              `json:"required"`
}

type causalRoundTripper struct {
	inner                           http.RoundTripper
	network, action, actor, exclude string
	replay                          []causalExchange
	mu                              sync.Mutex
	used                            map[int]bool
	prior, ordinal                  int
}

func secretKey(key string) bool {
	k := strings.ToLower(key)
	k = strings.NewReplacer("-", "", "_", "", ".", "", " ", "").Replace(k)
	sensitiveKeys := []string{
		"password", "passwd", "secret", "token", "authorization", "cookie", "email", "phone",
		"apikey", "publishablekey", "privatekey", "accesskey", "signingkey",
	}
	for _, s := range sensitiveKeys {
		if strings.Contains(k, s) {
			return true
		}
	}
	return false
}

func redactGo(value interface{}) interface{} {
	switch v := value.(type) {
	case map[string]interface{}:
		out := map[string]interface{}{}
		keys := make([]string, 0, len(v))
		for k := range v {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			if secretKey(k) {
				if s, ok := v[k].(string); ok {
					out[k] = fmt.Sprintf("<reproit:string:length=%d>", len([]rune(s)))
				} else {
					out[k] = "<reproit:secret>"
				}
			} else {
				out[k] = redactGo(v[k])
			}
		}
		return out
	case []interface{}:
		out := make([]interface{}, len(v))
		for i := range v {
			out[i] = redactGo(v[i])
		}
		return out
	default:
		return value
	}
}

func redactedHeaders(h http.Header) map[string]string {
	out := map[string]string{}
	for k, values := range h {
		if secretKey(k) {
			out[k] = "<reproit:secret>"
		} else {
			out[k] = strings.Join(values, ",")
		}
	}
	return out
}

func canonicalGoURL(raw string) string {
	u, err := url.Parse(raw)
	if err != nil {
		return raw
	}
	u.RawQuery = u.Query().Encode()
	return u.String()
}
func (c *causalRoundTripper) actionIndex() int {
	b, err := os.ReadFile(c.action)
	if err != nil {
		return 0
	}
	n, _ := strconv.Atoi(strings.TrimSpace(string(b)))
	return n
}

func (c *causalRoundTripper) RoundTrip(req *http.Request) (*http.Response, error) {
	if c.exclude != "" && strings.HasPrefix(req.URL.String(), c.exclude) {
		return c.inner.RoundTrip(req)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	action := c.actionIndex()
	if action != c.prior {
		c.prior = action
		c.ordinal = 0
	}
	ordinal := c.ordinal
	c.ordinal++
	if c.replay != nil {
		for i, e := range c.replay {
			matches := !c.used[i] && e.Required && e.Actor == c.actor &&
				e.ActionIndex == action && strings.EqualFold(e.Method, req.Method) &&
				canonicalGoURL(e.URL) == canonicalGoURL(req.URL.String())
			if matches {
				c.used[i] = true
				body, _ := json.Marshal(e.ResponseBody)
				if s, ok := e.ResponseBody.(string); ok {
					body = []byte(s)
				}
				h := http.Header{}
				for k, v := range e.ResponseHeaders {
					h.Set(k, v)
				}
				return &http.Response{
					StatusCode: e.Status,
					Status:     fmt.Sprintf("%d replay", e.Status),
					Header:     h,
					Body:       io.NopCloser(bytes.NewReader(body)),
					Request:    req,
				}, nil
			}
		}
		return nil, fmt.Errorf("CAPSULE:MISS %s %s action=%d", req.Method, req.URL, action)
	}
	var requestBody interface{}
	if req.Body != nil {
		raw, _ := io.ReadAll(req.Body)
		req.Body.Close()
		req.Body = io.NopCloser(bytes.NewReader(raw))
		if json.Unmarshal(raw, &requestBody) == nil {
			requestBody = redactGo(requestBody)
		} else if len(raw) > 0 {
			requestBody = fmt.Sprintf("<reproit:body:length=%d>", len(raw))
		}
	}
	resp, err := c.inner.RoundTrip(req)
	if err != nil {
		return nil, err
	}
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	resp.Body.Close()
	resp.Body = io.NopCloser(bytes.NewReader(raw))
	var responseBody interface{}
	isJSON := strings.Contains(resp.Header.Get("content-type"), "json")
	if isJSON && json.Unmarshal(raw, &responseBody) == nil {
		responseBody = redactGo(responseBody)
	} else {
		responseBody = fmt.Sprintf("<reproit:body:length=%d>", len(raw))
	}
	e := causalExchange{
		ID:              fmt.Sprintf("%s-%d-%d", c.actor, action, ordinal),
		Actor:           c.actor,
		ActionIndex:     action,
		Ordinal:         ordinal,
		Protocol:        req.URL.Scheme,
		Method:          req.Method,
		URL:             canonicalGoURL(req.URL.String()),
		RequestHeaders:  redactedHeaders(req.Header),
		RequestBody:     requestBody,
		Status:          resp.StatusCode,
		ResponseHeaders: redactedHeaders(resp.Header),
		ResponseBody:    responseBody,
		Required:        true,
	}
	if line, err := json.Marshal(e); err == nil {
		if f, err := os.OpenFile(c.network, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600); err == nil {
			fmt.Fprintln(f, string(line))
			f.Close()
		}
	}
	return resp, nil
}

// InstallCausalHTTP wraps http.DefaultTransport only while the Reproit runner
// has provisioned side files. It returns a restoration function.
func InstallCausalHTTP(excludePrefix string) func() {
	network, capsulePath := os.Getenv("REPROIT_NETWORK_FILE"), os.Getenv("REPROIT_CAPSULE")
	if network == "" && capsulePath == "" {
		return func() {}
	}
	var replay []causalExchange
	if capsulePath != "" {
		var cap struct {
			Exchanges []causalExchange `json:"exchanges"`
		}
		if raw, err := os.ReadFile(capsulePath); err == nil && json.Unmarshal(raw, &cap) == nil {
			replay = cap.Exchanges
		}
	}
	prior := http.DefaultTransport
	http.DefaultTransport = &causalRoundTripper{
		inner:   prior,
		network: network,
		action:  os.Getenv("REPROIT_ACTION_FILE"),
		actor:   envOr("REPROIT_DEVICE", "a"),
		exclude: excludePrefix,
		replay:  replay,
		used:    map[int]bool{},
	}
	mergeGoCapabilities(capsulePath != "")
	return func() { http.DefaultTransport = prior }
}

func envOr(key, fallback string) string {
	if value := os.Getenv(key); value != "" {
		return value
	}
	return fallback
}
func mergeGoCapabilities(replay bool) {
	path := os.Getenv("REPROIT_CAPABILITIES_FILE")
	if path == "" {
		return
	}
	value := map[string]interface{}{}
	if raw, err := os.ReadFile(path); err == nil {
		json.Unmarshal(raw, &value)
	}
	value["http"] = map[string]string{"status": "captured", "detail": "Go http.DefaultTransport"}
	value["http_replay"] = map[string]string{
		"status": "captured",
		"detail": "Go fail-closed RoundTripper",
	}
	raw, _ := json.Marshal(value)
	os.WriteFile(path, raw, 0600)
}
