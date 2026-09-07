"""Failure evidence helpers; importing them starts no daemon or engine."""
import json


def reject_launch(response, inspect_record, remember_record):
    """Retain the primary launch failure even if no agent was registered."""
    evidence = {"launch_response": response}
    try:
        record = inspect_record()
        if record.get("container"):
            remember_record(record)
    except Exception as error:
        evidence["cleanup_lookup_error"] = str(error)
    raise AssertionError(json.dumps(evidence, sort_keys=True))
