# windows-avalonia offline corpus

The native Windows Avalonia corpus completed with one clean case and two
adversarial cases. All three reached their live observation, reported no
defect identity, and removed their owned process, run root, configuration,
pointer movement, and firewall rules.

Each subject was offline during observation. The elevated interactive task
installed program-scoped Windows Firewall block rules for inbound and outbound
traffic, verified both active, launched the exact build, retained the
observation, and removed both rules in a `finally` block.

| Kind | Case | Native observation | Verdict |
|---|---|---|---|
| clean | PicView fixed corner | main window closed | no identity |
| adversarial | PicView affected button center | main window closed | legal close |
| adversarial | ILSpy affected independent fold | 251 active rows | legal fold |

The PicView adversarial case uses the same main window and close button as the
defect campaign, but clicks the legal button center instead of the defective
physical corner. The ILSpy case uses the same AvaloniaEdit peer, folding
margin, subject assembly, and rendered-row oracle as the defect campaign, but
exercises the independent XML documentation fold.

Raw JSON remains at `C:\lab\campaigns\avalonia\corner-corpus` in the native VM.
The three records concatenated in lexical filename order hash to
`866da324e0a1858652f1f6061ed7408a448a082223281fd7044cc12f29cfecc9`.
