# Module under test for the Ruby flaky-CI fixture: computes an order total
# from the tax rate the config service answers.
#
# The planted bug: the config service's LEGACY format returns the rate as a
# STRING ("0.25"). `1 + rate` then raises TypeError (String can't be coerced
# into Integer), so the total is never computed and the test errors. FIXED=1
# applies the fix: coerce the rate to a number before arithmetic.

require "json"
require "net/http"

def order_total(subtotal, config_url)
  response = Net::HTTP.get_response(URI(config_url + "/tax-rate"))
  rate = JSON.parse(response.body)["rate"]
  applied = ENV["FIXED"] == "1" ? Float(rate) : rate
  subtotal * (1 + applied)
end
