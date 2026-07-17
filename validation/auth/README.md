# Auth discovery validation corpus

Fixtures in this directory model mapped login UIs used to validate automatic journey inference. They
must contain no real credentials. Coverage should grow across password, multi-screen OTP, split OTP
inputs, passkeys, account pickers, and post-login workspace selection. A generated journey is never
accepted only because it matches a fixture: the CLI also executes a clean verification run.
