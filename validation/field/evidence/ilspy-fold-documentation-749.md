# windows-avalonia field campaign: ilspy-fold-documentation-749

The selected ILSpy folding defect reproduced three times on native x86_64
Windows, and the exact fixed revision removed it three times.

- Affected revision: `48fb85960e2adce0367ba925d3f2bf1f6b0384f9`
- Fixed revision: `800efc6e105ce4a94f25a335938c53927f3cb4b6`
- Automation: Windows UI Automation in interactive session 1
- Editor peer: AvaloniaEdit automation id `self`
- Subject: a pinned local assembly with XML documentation
- Network: blocked inbound and outbound for the exact executable

The harness expands the method body and XML documentation, opens the body
context menu, locates `Toggle Folding` through UIA, and clicks that menu item.
Rendered text-row counts distinguish the states because the AvaloniaEdit peer
does not expose its full text through a standard UIA text pattern.

| Role | Run | Rows before | Rows after | Result |
|---|---:|---:|---:|---|
| affected | 1 | 233 | 230 | documentation remains expanded |
| affected | 2 | 233 | 230 | documentation remains expanded |
| affected | 3 | 233 | 230 | documentation remains expanded |
| fixed | 1 | 233 | 50 | documentation and body collapse |
| fixed | 2 | 233 | 50 | documentation and body collapse |
| fixed | 3 | 233 | 50 | documentation and body collapse |
| affected control | 1 | n/a | 251 | independent fold expands |

Every run verified exact application and subject hashes, both active firewall
block rules, interactive UIA readiness, a reached rendered observation, zero
remaining ILSpy processes, removal of the firewall rules and run root, and
pointer restoration.

Raw JSON remains at `C:\lab\campaigns\avalonia\evidence` in the native VM.
The seven ILSpy records concatenated in lexical filename order hash to
`ed0199f3c43dff6d0163773972b51a24879f9af11e433f46ce880b16ef5ca258`.
