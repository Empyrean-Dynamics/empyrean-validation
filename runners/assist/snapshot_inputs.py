"""Sealed-snapshot input errors for the ASSIST runner.

The ASSIST runner reads its Horizons state vectors from the pinned inputs
snapshot on disk (the same ``fixtures/horizons`` tree the rust reference
writes) and from nowhere else: CI points the JPL and MPC hosts at the loopback
address before running, so there is no network to fall back to. A cache miss
is therefore a gap in the pinned inputs, not a cue to fetch, and must fail the
leg loudly rather than skip the object.

Kept dependency-free (standard library only, no ``assist`` / ``rebound``
import) so the message construction is unit-testable on its own — run this
file directly for the self-check (``python3 -I snapshot_inputs.py``).
"""

from __future__ import annotations


class MissingInputError(RuntimeError):
    """A JPL/MPC product the pinned inputs snapshot does not contain.

    Raised instead of fetching: the validation inputs are sealed, so a miss
    fails the leg, naming the object, the product, the lookup key and the
    directory searched.
    """


def missing_input_message(object_name: str, product: str, key: str, cache_dir) -> str:
    """Build the fatal message for an input the pinned snapshot does not carry."""
    return (
        f"{object_name}: {product} not in the pinned inputs snapshot "
        f"(key '{key}' under {cache_dir}): sealed-snapshot cache miss. "
        "The validation inputs are sealed from JPL and the MPC; re-mint the "
        "snapshot to add this input."
    )


def _self_check() -> None:
    msg = missing_input_message(
        "Apophis", "Horizons vectors", "DES=99942;|60000.00000000", "fixtures/horizons"
    )
    assert "Apophis" in msg, msg
    assert "Horizons vectors" in msg, msg
    assert "DES=99942;|60000.00000000" in msg, msg
    assert "fixtures/horizons" in msg, msg
    assert "sealed" in msg, msg
    # MissingInputError is a plain RuntimeError subclass carrying that message.
    err = MissingInputError(msg)
    assert isinstance(err, RuntimeError)
    assert str(err) == msg
    print("snapshot_inputs self-check OK")


if __name__ == "__main__":
    _self_check()
