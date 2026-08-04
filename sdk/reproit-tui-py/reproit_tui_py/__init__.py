"""reproit-tui-py: the production telemetry SDK a Python terminal-UI app embeds.

Computes the canonical TUI screen signature (a port of crates/tui-sig), tracks
coverage edges, and reports sessions + crash signatures to the reproit cloud.
TUI signatures live in a SEPARATE namespace from the a11y signatures (see
signature.py and README.md). No em dashes anywhere, per project rules.
"""

from .signature import (
    sig_of,
    structural_class,
    skeleton_of,
    structural_sig,
    content_fingerprint,
    value_class,
    is_strict_decimal,
    numeric_value_classes,
    labels_of,
    MAX_VALUE_CLASSES,
    MAX_LABELS,
)
from .capture import ScreenContents, Cell
from .reporter import Reporter, auto_context
from .causal import install_causal_urllib

__all__ = [
    "sig_of",
    "structural_class",
    "skeleton_of",
    "structural_sig",
    "content_fingerprint",
    "value_class",
    "is_strict_decimal",
    "numeric_value_classes",
    "labels_of",
    "MAX_VALUE_CLASSES",
    "MAX_LABELS",
    "ScreenContents",
    "Cell",
    "Reporter",
    "auto_context",
    "install_causal_urllib",
]

__version__ = "1.0.0"
