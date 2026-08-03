// What mounting the Go adapter actually costs a request.
//
// The same method as adapter-benchmark.mjs, so the three numbers are
// comparable: a real net/http server over a real socket, driven in four
// shapes with keep-alive on, measured in ALTERNATING rounds.
//
//	baseline  the handler alone
//	inactive  adapter mounted, request carries no trace context (the shape
//	          almost every production request has)
//	active    adapter mounted, request carries `x-reproit-trace`
//	control   a second baseline, measured apart from the first
//
// HTTP, socket and JSON costs are present in all four, so subtracting the
// baseline leaves the adapter. The gap between the two baselines is the
// method's own noise floor, reported so nobody reads a number smaller than it
// as signal: a single pass per shape once put an inactive adapter at a
// NEGATIVE cost, which is drift, not a result.
package main

import (
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"sort"
	"strconv"
	"time"

	reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"
)

func envInt(name string, fallback int) int {
	if raw := os.Getenv(name); raw != "" {
		if parsed, err := strconv.Atoi(raw); err == nil && parsed > 0 {
			return parsed
		}
	}
	return fallback
}

// Ceilings, not targets, and sized for a shared CI runner rather than a
// developer laptop. A gate that flakes gets ignored, and an ignored gate
// measures nothing; these sit far above the local numbers so ordinary
// contention cannot fail a build, while an adapter that started doing real
// per-request work still would.
const (
	noiseCeilingMicros    = 120.0
	inactiveCeilingMicros = 120.0
	activeCeilingMicros   = 400.0
)

type account struct {
	ID int  `json:"id"`
	OK bool `json:"ok"`
}

func handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_ = r.URL.Query().Get("id")
		w.Header().Set("content-type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]account{"account": {ID: 42, OK: true}})
	})
}

func serve(mounted bool) (*http.Server, string, error) {
	var served http.Handler = handler()
	if mounted {
		served = reproit.Middleware(reproit.MiddlewareOptions{})(served)
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, "", err
	}
	server := &http.Server{Handler: served}
	go func() { _ = server.Serve(listener) }()
	return server, listener.Addr().String(), nil
}

// One shape, measured as microseconds per request. Keep-alive is on and a
// single connection is used, because otherwise connection setup dominates and
// the adapter disappears into the noise, which would flatter the result rather
// than measure it.
func measure(mounted bool, traced bool, runs int, warmup int) (float64, error) {
	server, address, err := serve(mounted)
	if err != nil {
		return 0, err
	}
	defer func() { _ = server.Close() }()
	client := &http.Client{Transport: &http.Transport{
		MaxIdleConns:        1,
		MaxIdleConnsPerHost: 1,
		MaxConnsPerHost:     1,
	}}
	defer client.CloseIdleConnections()

	fire := func() error {
		request, err := http.NewRequest(http.MethodGet, "http://"+address+"/account?id=42", nil)
		if err != nil {
			return err
		}
		if traced {
			request.Header.Set("x-reproit-trace", "bench-trace")
		}
		response, err := client.Do(request)
		if err != nil {
			return err
		}
		_, _ = io.Copy(io.Discard, response.Body)
		return response.Body.Close()
	}
	for index := 0; index < warmup; index++ {
		if err := fire(); err != nil {
			return 0, err
		}
	}
	started := time.Now()
	for index := 0; index < runs; index++ {
		if err := fire(); err != nil {
			return 0, err
		}
	}
	return float64(time.Since(started).Microseconds()) / float64(runs), nil
}

func median(values []float64) float64 {
	sorted := append([]float64(nil), values...)
	sort.Float64s(sorted)
	middle := len(sorted) / 2
	if len(sorted)%2 == 1 {
		return sorted[middle]
	}
	return (sorted[middle-1] + sorted[middle]) / 2
}

func abs(value float64) float64 {
	if value < 0 {
		return -value
	}
	return value
}

func round2(value float64) float64 {
	return float64(int64(value*100+0.5)) / 100
}

func main() {
	runs := envInt("REPROIT_ADAPTER_BENCH_RUNS", 3000)
	rounds := envInt("REPROIT_ADAPTER_BENCH_ROUNDS", 5)
	warmup := runs / 4
	if warmup > 500 {
		warmup = 500
	}

	samples := map[string][]float64{}
	for round := 0; round < rounds; round++ {
		for _, shape := range []struct {
			name    string
			mounted bool
			traced  bool
		}{
			{"baseline", false, false},
			{"inactive", true, false},
			{"active", true, true},
			{"control", false, false},
		} {
			micros, err := measure(shape.mounted, shape.traced, runs, warmup)
			if err != nil {
				fmt.Fprintf(os.Stderr, "benchmark failed on %s: %v\n", shape.name, err)
				os.Exit(1)
			}
			samples[shape.name] = append(samples[shape.name], micros)
		}
	}

	baseline := median(samples["baseline"])
	inactive := median(samples["inactive"])
	active := median(samples["active"])
	// Two identical shapes measured apart: whatever separates them is noise,
	// so a smaller difference cannot be called a cost.
	noiseFloor := abs(median(samples["control"]) - baseline)
	inactiveCost := inactive - baseline
	activeCost := active - baseline

	failures := []string{}
	if noiseFloor >= noiseCeilingMicros {
		failures = append(failures, fmt.Sprintf(
			"the method's own noise is %.2fus, too loud for this run to mean anything", noiseFloor))
	}
	if inactiveCost >= inactiveCeilingMicros {
		failures = append(failures, fmt.Sprintf(
			"inactive adapter adds %.2fus per request, over the %.0fus ceiling",
			inactiveCost, inactiveCeilingMicros))
	}
	if activeCost >= activeCeilingMicros {
		failures = append(failures, fmt.Sprintf(
			"active adapter adds %.2fus per request, over the %.0fus ceiling",
			activeCost, activeCeilingMicros))
	}

	report, err := json.Marshal(map[string]any{
		"language":                "go",
		"runs":                    runs,
		"rounds":                  rounds,
		"noiseFloorMicros":        round2(noiseFloor),
		"baselineMicros":          round2(baseline),
		"inactiveMicros":          round2(inactive),
		"activeMicros":            round2(active),
		"inactiveCostMicros":      round2(inactiveCost),
		"activeCostMicros":        round2(activeCost),
		"inactiveBelowNoiseFloor": inactiveCost < noiseFloor,
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "reporting failed: %v\n", err)
		os.Exit(1)
	}
	fmt.Println(string(report))
	if len(failures) > 0 {
		for _, failure := range failures {
			fmt.Fprintln(os.Stderr, failure)
		}
		os.Exit(1)
	}
}
