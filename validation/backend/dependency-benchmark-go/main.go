package main

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strconv"
	"time"

	reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"
)

const dependencies = 64

func envInt(name string, fallback int) int {
	value, error := strconv.Atoi(os.Getenv(name))
	if error == nil && value > 0 {
		return value
	}
	return fallback
}

func measure(runs int, captured bool) float64 {
	context := &reproit.TraceContext{TraceID: "dependency-benchmark", ActionIndex: 1}
	exchange := map[string]any{
		"request":  map[string]any{"method": "GET", "url": "http://pricing.test/quote?tier=gold"},
		"response": map[string]any{"status": 200, "body": map[string]any{"price": 42}},
	}
	started := time.Now()
	for run := 0; run < runs; run++ {
		trace, error := reproit.Begin(context, "dependencyBenchmark", reproit.BeginOptions{})
		if error != nil {
			panic(error)
		}
		if captured {
			for index := 0; index < dependencies; index++ {
				error = trace.Exchange(reproit.EffectCall, reproit.ExchangeOptions{
					Resource: "pricing", Key: strconv.Itoa(index), Exchange: exchange,
				})
				if error != nil {
					panic(error)
				}
			}
		}
	}
	return float64(time.Since(started).Nanoseconds()) / 1000 / float64(runs*dependencies)
}

func median(values []float64) float64 {
	values = append([]float64(nil), values...)
	sort.Float64s(values)
	return values[len(values)/2]
}

func main() {
	runs := envInt("REPROIT_DEPENDENCY_BENCH_RUNS", 300)
	rounds := envInt("REPROIT_DEPENDENCY_BENCH_ROUNDS", 7)
	samples := map[string][]float64{}
	for round := 0; round < rounds; round++ {
		samples["baseline"] = append(samples["baseline"], measure(runs, false))
		samples["captured"] = append(samples["captured"], measure(runs, true))
		samples["control"] = append(samples["control"], measure(runs, false))
	}
	baseline := median(samples["baseline"])
	cost := median(samples["captured"]) - baseline
	noise := median(samples["control"]) - baseline
	if noise < 0 {
		noise = -noise
	}
	if noise >= 10 || cost >= 50 {
		fmt.Fprintf(
			os.Stderr,
			"go dependency benchmark outside ceiling: noise=%.2f cost=%.2f\n",
			noise,
			cost,
		)
		os.Exit(1)
	}
	report, _ := json.Marshal(map[string]any{
		"language": "go", "runs": runs, "rounds": rounds,
		"dependenciesPerTrace": dependencies, "noiseFloorMicros": noise,
		"captureCostMicros": cost, "ceilingMicros": 50,
	})
	fmt.Println(string(report))
}
