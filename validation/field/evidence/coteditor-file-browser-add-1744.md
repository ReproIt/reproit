# CotEditor issue 1744 macOS Accessibility evidence

The exact affected and fixed commits were built from
`https://github.com/coteditor/CotEditor` with Xcode 26.2 on macOS 26.1 arm64.
The build uses the repository's ad hoc signing configuration and disables the
hardened runtime for local launch. The affected setup build completed in 42
seconds.

Xcode 26.2 also requires one build-only package correction at these revisions:
the `ControlUI` target imports `URLUtils`, so the campaign declares the already
present `EditorCore` `URLUtils` product as its direct dependency. The correction
and signing selection are applied identically to both revisions. Neither change
touches `FileBrowserViewController.swift`, the issue fix, localization, runtime
state, or any observed AX attribute.

The minimized trigger opens a fresh folder containing `document.txt`, opens
that file in the same window so the editor has focus, presses the only
`AXMenuButton` in the file browser, and reads `AXEnabled` from the `New File`
`AXMenuItem`. The value was false in all three affected runs and true in all
three fixed runs.

The neighboring legal control leaves the folder open with no document selected
and performs the same menu action. `New File` was enabled on both the affected
and fixed builds. This proves the identity includes the document-focus state and
does not classify the Add menu or the affected binary in general as broken.

Every run used a copied and re-signed app with a unique bundle identifier, a
unique workspace, and a unique `CFFIXED_USER_HOME`. The proxy and process
containment were the same as the Platypus campaign. No run established an
external connection. The structured evidence retains the three affected
reproductions, three fixed controls, exact boolean values, and replay
measurements. Manual review confirmed the identity against issue 1744.
