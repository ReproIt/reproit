package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"strconv"
)

var mode = os.Getenv("REPROIT_FIXTURE_MODE")

const tag = `"fixture-v1"`

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func main() {
	if mode == "" {
		mode = "clean"
	}
	port := os.Getenv("PORT")
	if port == "" {
		port = "19480"
	}
	http.HandleFunc("/health", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, 200, map[string]bool{"ready": true})
	})
	http.HandleFunc("/codec", func(w http.ResponseWriter, r *http.Request) {
		typed := r.URL.Query().Get("value")
		decoded := typed
		if mode == "broken" {
			if n, err := strconv.ParseFloat(typed, 64); err == nil {
				decoded = strconv.FormatFloat(n, 'f', -1, 64)
			}
		}
		if mode == "incomplete" {
			writeJSON(w, 200, map[string]string{})
			return
		}
		writeJSON(w, 200, map[string]string{"decoded": decoded})
	})
	http.HandleFunc("/representation", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("ETag", tag)
		w.Header().Set("Vary", "accept-language")
		if r.Header.Get("If-None-Match") == tag {
			if mode == "broken" {
				_, _ = w.Write([]byte("contradictory-v2"))
				return
			}
			w.WriteHeader(304)
			return
		}
		w.Header().Set("Content-Type", "text/plain")
		_, _ = w.Write([]byte("authoritative-v1"))
	})
	http.HandleFunc("/media", func(w http.ResponseWriter, _ *http.Request) {
		if mode == "incomplete" {
			w.WriteHeader(200)
			return
		}
		if mode != "incomplete" {
			w.Header().Set("Content-Type", "application/json")
		}
		if mode == "broken" {
			_, _ = w.Write([]byte("{invalid-json"))
		} else {
			_, _ = w.Write([]byte(`{"ok":true}`))
		}
	})
	http.HandleFunc("/lifecycle", func(w http.ResponseWriter, _ *http.Request) {
		names := []string{"request.start", "callback", "request.close"}
		if mode == "broken" {
			names = []string{"request.start", "request.close", "callback"}
		}
		events := make([]map[string]any, len(names))
		for index, name := range names {
			events[index] = map[string]any{
				"sequence": index + 1,
				"name":     name,
				"scopeId":  "scope-1",
			}
		}
		writeJSON(w, 200, map[string]any{
			"complete":  mode != "incomplete",
			"scopeKind": "request",
			"scopeId":   "scope-1",
			"events":    events,
		})
	})
	fmt.Printf("READY %s\n", port)
	if err := http.ListenAndServe("127.0.0.1:"+port, nil); err != nil {
		panic(err)
	}
}
