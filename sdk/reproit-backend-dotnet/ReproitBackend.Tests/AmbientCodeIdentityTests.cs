// Keep this suite hermetic against the CI environment, and pin the fallback it
// would otherwise exercise only by accident.
//
// Capture.ResolveCommit falls back to REPROIT_COMMIT then GITHUB_SHA, which is
// correct: a deployment should carry its code identity without being told
// twice. But a GitHub runner always sets GITHUB_SHA and a laptop never does, so
// any test asserting an exact `deployment` shape passes locally and fails in CI.
//
// The Python, Java and Ruby SDKs each hit exactly that, separately, and the last
// two only once SDK support tiers were abolished and their suites began gating.
// No .NET test asserts that shape today, so this file exists to keep the latent
// case latent: it proves the seam works, so a future exact-shape assertion has
// something to neutralize the environment with.
using System;
using ReproitBackend;
using Xunit;

public class AmbientCodeIdentityTests
{
    [Fact]
    public void AConfiguredCommitWinsOverTheAmbientEnvironment()
    {
        var saved = Capture.ReadEnvironment;
        try
        {
            Capture.ReadEnvironment = name =>
                name == "GITHUB_SHA" ? "f857cb7740a5f857cb7740a5f857cb7740a5f857" : null;
            var config = new CaptureConfig
            {
                Endpoint = "http://127.0.0.1:9/v1/events",
                ApiKey = "sk",
                AppId = "app-demo",
                Commit = "0123456789abcdef0123456789abcdef01234567",
            };
            Assert.Equal("0123456789abcdef0123456789abcdef01234567", Capture.ResolveCommit(config));
        }
        finally
        {
            Capture.ReadEnvironment = saved;
        }
    }

    [Fact]
    public void ACiRunnerSuppliesTheCommitTheConfigOmits()
    {
        var saved = Capture.ReadEnvironment;
        try
        {
            const string Sha = "f857cb7740a5f857cb7740a5f857cb7740a5f857";
            Capture.ReadEnvironment = name => name == "GITHUB_SHA" ? Sha : null;
            var config = new CaptureConfig
            {
                Endpoint = "http://127.0.0.1:9/v1/events",
                ApiKey = "sk",
                AppId = "app-demo",
            };
            Assert.Equal(Sha, Capture.ResolveCommit(config));
        }
        finally
        {
            Capture.ReadEnvironment = saved;
        }
    }

    [Fact]
    public void AnEmptyEnvironmentYieldsNoCommit()
    {
        var saved = Capture.ReadEnvironment;
        try
        {
            Capture.ReadEnvironment = _ => null;
            var config = new CaptureConfig
            {
                Endpoint = "http://127.0.0.1:9/v1/events",
                ApiKey = "sk",
                AppId = "app-demo",
            };
            Assert.Null(Capture.ResolveCommit(config));
        }
        finally
        {
            Capture.ReadEnvironment = saved;
        }
    }
}
