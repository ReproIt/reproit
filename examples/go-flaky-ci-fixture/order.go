// Module under test for the Go flaky-CI fixture: computes an order total
// from the tax rate the config service answers.
//
// The planted bug: the config service's LEGACY format returns the rate as a
// STRING ("0.25"). The legacy branch parses it with strconv.Atoi and IGNORES
// the error, so "0.25" reads as 0 and a 100 subtotal totals 100 instead of
// 125. FIXED=1 applies the fix: strconv.ParseFloat before arithmetic.
package order

import (
	"context"
	"encoding/json"
	"net/http"
	"os"
	"strconv"
)

// OrderTotal fetches the tax rate from the config service and applies it.
func OrderTotal(
	ctx context.Context,
	client *http.Client,
	configURL string,
	subtotal float64,
) (float64, error) {
	request, err := http.NewRequestWithContext(
		ctx, http.MethodGet, configURL+"/tax-rate", nil)
	if err != nil {
		return 0, err
	}
	response, err := client.Do(request)
	if err != nil {
		return 0, err
	}
	defer func() { _ = response.Body.Close() }()
	var body map[string]any
	if err := json.NewDecoder(response.Body).Decode(&body); err != nil {
		return 0, err
	}
	rate := 0.0
	switch value := body["rate"].(type) {
	case float64:
		rate = value
	case string:
		if os.Getenv("FIXED") == "1" {
			parsed, err := strconv.ParseFloat(value, 64)
			if err != nil {
				return 0, err
			}
			rate = parsed
		} else {
			// The planted bug: legacy rates are strings, Atoi cannot parse
			// "0.25", and the ignored error leaves the rate at zero.
			parsed, _ := strconv.Atoi(value)
			rate = float64(parsed)
		}
	}
	return subtotal * (1 + rate), nil
}
