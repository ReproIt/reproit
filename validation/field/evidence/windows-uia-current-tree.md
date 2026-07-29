# Windows UI Automation current-tree diagnostic

This record covers the owned native Windows UI Automation gate on the shared
candidate working tree. It is diagnostic evidence, not exact-commit promotion
evidence.

## Source and execution

- Recorded base commit:
  `f696236f3d89f24d08f454e6e2e741348dae4263`
- Candidate source archive SHA-256:
  `5186884b33ce15dfda0ecd44ae9480fde8df932d6e0c4586f5cba538b4f8be0b`
- Native worker: Windows x86_64 VM reached through
  `black@zgx-5a09.local`, `strix`, and the forwarded QEMU guest
- Guest operating system: Microsoft Windows NT 10.0.26100.0
- Harness: `validation/backends/run-windows-desktop.ps1`
- Result: passed
- Retained guest log:
  `C:\lab\evidence-current-wpf\windows-current-tree-pass.log`
- Log SHA-256:
  `cae3afe1db6fcd3931141c3edbbabf30984429a797cfb1ad84c2006fd2393a2b`
- Log size: 17,595 bytes

The source archive was built from the complete tracked and untracked candidate
tree and verified to contain no macOS AppleDouble files before execution. The
run used a process-specific Cargo target directory so parallel or stale
Windows runs cannot share build-script output.

## Native results

The gate published and launched each real desktop fixture in the interactive
Windows session, attached through Windows UI Automation, observed its initial
state, invoked Toggle, observed the changed state, invoked Reset, and observed
the restored state.

The retained log contains all required journey and result markers for:

- WPF: `WPF UI Automation runtime passed`
- Avalonia: `Avalonia UI Automation runtime passed`
- WinUI 3: `WinUI UI Automation runtime passed`
- Final result:
  `Windows DesktopUia backend passed WPF, Avalonia, and WinUI`

Each framework produced `EXPLORE:STATE`, `EXPLORE:EDGE`, `JOURNEY DONE`, and
`All tests passed`.

## Root-cause correction

The first exact-commit attempt reached the Windows guest but failed during the
release build because all invocations reused a fixed directory under
`%TEMP%\reproit-backend-target`. A concurrent or interrupted build could then
remove or replace tree-sitter build-script output while another Cargo process
was reading it.

The Windows runner now assigns
`%TEMP%\reproit-backend-target-$PID`, creates it before the build, and removes
it in `finally`. A contract test checks both the process-specific name and the
cleanup path. The passing native diagnostic used this corrected runner.

## Cleanup

After the passing result, the guest reported:

- fixture processes: 0
- process-specific Cargo target directories: 0
- owned scheduled task present: false
- owned source, archive, batch, and completion-marker paths remaining: 0

The verified log was retained separately from the disposable execution root.
The shared repository had uncommitted candidate changes during this run, so
the recorded Git commit does not identify the exact tree that executed. The
canonical exact-commit remote gate must be rerun after the shared changes are
committed before any Windows target can use this result for Stable promotion.
