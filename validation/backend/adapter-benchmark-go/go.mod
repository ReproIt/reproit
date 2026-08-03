// A separate module so the benchmark lives next to its Node and Python
// siblings instead of inside the SDK it measures. The replace directive points
// at the checkout, so it always measures THIS tree, never a published copy.
module github.com/ReproIt/reproit/validation/backend/adapter-benchmark-go

go 1.26.3

require github.com/ReproIt/reproit/sdk/reproit-backend-go v0.0.0

replace github.com/ReproIt/reproit/sdk/reproit-backend-go => ../../../sdk/reproit-backend-go
