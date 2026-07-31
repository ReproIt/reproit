"""Measure whether CUDA computation is bit reproducible.

The CUDA counterpart of mps_determinism.py, deliberately the SAME probes in the
same order so the two backends can be compared line for line. The questions are
identical and still distinct:

  within-process   same seed, same op, repeated in one process
  across-process   same seed, same op, in a fresh process

A capsule replays in a FRESH process, so across-process reproducibility is the
one that decides whether a training-shaped workload can be replayed at all.

CUDA differs from Metal in one way that matters to a capsule: PyTorch documents
`use_deterministic_algorithms(True)` as RAISING for an op with no deterministic
implementation, and `scatter_add_` on floating point is such an op. So on this
backend the flag is expected to be load bearing rather than cosmetic. Whether it
actually is, is the thing being measured rather than assumed. CUBLAS_WORKSPACE_CONFIG
is also read, because cuBLAS reductions can be nondeterministic without it.

Prints one JSON object so the harness can aggregate across runs.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys


def digest(tensor) -> str:
    """Bit-exact fingerprint of a tensor's bytes, taken on the CPU."""
    raw = tensor.detach().to("cpu").contiguous()
    return hashlib.sha256(bytes(raw.view(-1).to("cpu").numpy().data)).hexdigest()[:16]


def main() -> int:
    try:
        import torch
    except Exception as error:  # noqa: BLE001
        print(json.dumps({"available": False, "reason": str(error)}))
        return 0

    if not torch.cuda.is_available():
        print(json.dumps({"available": False, "reason": "cuda not available"}))
        return 0

    device = torch.device("cuda")
    results: dict[str, object] = {
        "available": True,
        "torch": torch.__version__,
        "device": torch.cuda.get_device_name(0),
        "capability": ".".join(str(part) for part in torch.cuda.get_device_capability(0)),
        # Recorded because it changes cuBLAS reduction behaviour and a capsule
        # that omitted it would be replaying under a different contract.
        "cublas_workspace_config": os.environ.get("CUBLAS_WORKSPACE_CONFIG", "<unset>"),
    }

    # 1. Seeded random generation.
    torch.manual_seed(1234)
    a = torch.randn(1024, 1024, device=device)
    results["seeded_randn"] = digest(a)

    # 2. Matmul, the workhorse, on fixed inputs.
    torch.manual_seed(1234)
    x = torch.randn(512, 512, device=device)
    y = torch.randn(512, 512, device=device)
    results["matmul"] = digest(x @ y)

    # 3. A large reduction. Floating point addition is not associative, so a
    #    reduction whose order varies is the classic source of run to run drift.
    torch.manual_seed(7)
    big = torch.randn(4_000_000, device=device)
    results["reduction_sum"] = digest(big.sum())

    # 4. Repeat the same reduction in THIS process, to separate within-process
    #    from across-process behaviour.
    results["reduction_sum_repeat"] = digest(big.sum())

    # 5. scatter_add, which is order dependent on parallel hardware and is the
    #    op deterministic-mode flags usually target.
    torch.manual_seed(11)
    src = torch.randn(200_000, device=device)
    index = torch.randint(0, 1000, (200_000,), device=device)
    out = torch.zeros(1000, device=device)
    out.scatter_add_(0, index, src)
    results["scatter_add"] = digest(out)

    # 6. Does the deterministic flag even apply here? On CUDA the documented
    #    behaviour is to RAISE for an op with no deterministic kernel, which is
    #    an honest answer and the opposite of silently doing nothing.
    try:
        torch.use_deterministic_algorithms(True)
        results["deterministic_mode"] = "accepted"
    except Exception as error:  # noqa: BLE001
        results["deterministic_mode"] = f"rejected: {error}"
    results["deterministic_reported"] = bool(torch.are_deterministic_algorithms_enabled())
    try:
        torch.manual_seed(11)
        src2 = torch.randn(200_000, device=device)
        index2 = torch.randint(0, 1000, (200_000,), device=device)
        out2 = torch.zeros(1000, device=device)
        out2.scatter_add_(0, index2, src2)
        results["scatter_add_deterministic"] = digest(out2)
    except Exception as error:  # noqa: BLE001
        results["scatter_add_deterministic"] = f"raised: {type(error).__name__}"

    print(json.dumps(results))
    return 0


if __name__ == "__main__":
    sys.exit(main())
