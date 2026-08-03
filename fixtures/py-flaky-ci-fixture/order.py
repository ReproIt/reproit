"""Module under test for the Python flaky-CI fixture: computes an order total
from the tax rate the config service answers.

The planted bug: the config service's LEGACY format returns the rate as a
percent STRING ({"rate": "25", "unit": "percent"}). The buggy code coerces
the number and ignores the unit, so a 100 subtotal totals 2600.0 instead of
125.0. FIXED=1 applies the fix: honor the unit field before arithmetic.
"""

import json
import os
import urllib.request


def order_total(subtotal, config_url):
    with urllib.request.urlopen(config_url + "/tax-rate") as response:
        answer = json.loads(response.read().decode("utf-8"))
    rate = float(answer["rate"])
    if os.environ.get("FIXED") == "1" and answer.get("unit") == "percent":
        rate /= 100.0
    return subtotal * (1 + rate)
