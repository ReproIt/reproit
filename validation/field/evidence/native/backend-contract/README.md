# Backend contract exact native evidence

This directory retains the canonical `backend-contract` native release gate for
commit `9f1815ee26c35bf5d1b9b36ae7713b325b98317a`.

- Executor: native Linux x86_64
- Docker engine: Linux x86_64
- Source mode: exact, clean commit match
- Result: `backend-contract.json`
- Captured log: `backend-contract.log`
- Validated summary: `validated-summary.json`
- Run metadata: `run-metadata.json`
- Result SHA-256:
  `44a231930cbbe79c2853ab10f969cc486bcb9a94b267077919dbd019526d6334`
- Log SHA-256:
  `cb769ea7ad161967e84223a0e715312242dbb6e07e2a6a5dc7b93e42a4a6d1c3`
- Validated summary SHA-256:
  `85d0b99477f10e048e504cb473caae07c3f9e0705891e60f075a671125107028`
- Run metadata SHA-256:
  `afae6b7a8ff7b7f1a628d768dccd4044b64a0484fb94d037b5edfc6ccdce7c73`

The exact isolated gate passed the real CLI backend contract journey and its
required output marker. The cleanup audit found no owned remote run directory,
worker image, hosted container, or local standalone clone.
