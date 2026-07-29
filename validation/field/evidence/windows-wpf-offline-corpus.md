# windows-wpf offline corpus

The native Windows WPF corpus completed with one clean case and two
adversarial cases. All three reached the live UI Automation observation point,
reported no defect identity, and left no process, portable profile, scheduled
task, or firewall rule behind.

Each application was offline during its observation. The elevated interactive
corpus task installed program-scoped Windows Firewall block rules for both
inbound and outbound traffic, verified both rules active, launched the exact
application build, retained the observation, and removed the rules in a
`finally` block.

| Kind | Case | Native observation | Verdict |
|---|---|---|---|
| clean | Flow fixed, fr-FR | Bienvenue dans Flow Launcher | no identity |
| adversarial | Flow affected, en-US | Welcome to Flow Launcher | legal English |
| adversarial | ScreenToGif adjacent Shapes | Ajouter des formes (Alt + J) | legal neighbor |

The first adversarial case resembles the affected English title but is legal
for the selected culture. The second uses the same custom WPF ribbon, pointer
hover, and tooltip path as the defect while targeting an unchanged adjacent
command.

Raw records remain at `C:\lab\campaigns\windows-wpf-corpus` in the native VM.
Their byte content, concatenated in lexical filename order, hashes to
`7e5193ddcd70605c4a9bd9ed0a8734ca19b05ea17d0eff23753624867d636aef`.
