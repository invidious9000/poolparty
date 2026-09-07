"""Synthetic design exercise. No network, credentials, or upstream payloads."""

import codecs
import json
import math
import unittest


def remaining_ratio(*, used=None, remaining=None, total=None):
    """Missing/invalid evidence stays unknown; zero is meaningful when supplied."""
    values = (used, remaining, total)
    if any(v is not None and (type(v) not in (int, float) or not math.isfinite(v))
           for v in values):
        return None
    if total is None or total <= 0 or (used is None and remaining is None):
        return None
    if used is not None and not 0 <= used <= total:
        return None
    if remaining is not None and not 0 <= remaining <= total:
        return None
    if used is not None and remaining is not None and used + remaining != total:
        return None
    return remaining / total if remaining is not None else (total - used) / total


def ambiguous_remaining(*, count, total, percent=None):
    """A versionless count alone cannot say whether it is consumed or remaining."""
    if remaining_ratio(remaining=count, total=total) is None:
        return None
    if type(percent) not in (int, float) or not math.isfinite(percent):
        return None
    if not 0 <= percent <= 100:
        return None
    candidates = {count, total - count}
    matches = [n for n in candidates if abs(n / total * 100 - percent) <= 1]
    return matches[0] if len(matches) == 1 else None


class StreamObservation:
    """Observe LF/CRLF fixture streams while relaying original bytes unchanged.

    Deliberately limited: this is not a general SSE parser or production relay.
    It does not implement limits, backpressure, deadlines or protocol validation.
    """

    def __init__(self):
        self.decoder = codecs.getincrementaldecoder("utf-8")()
        self.pending = ""
        self.frame_lines = []
        self.events = []
        self.terminal = False
        self.error = False
        self.relayed = bytearray()

    def feed(self, chunk):
        self.relayed.extend(chunk)
        self.pending += self.decoder.decode(chunk)
        while "\n" in self.pending:
            line, self.pending = self.pending.split("\n", 1)
            line = line.removesuffix("\r")
            if line:
                self.frame_lines.append(line)
                continue
            data = "\n".join(
                line[5:].removeprefix(" ")
                for line in self.frame_lines if line.startswith("data:")
            )
            self.frame_lines = []
            if not data:
                continue
            event = json.loads(data)
            self.events.append(event)
            self.terminal |= event.get("type") == "message_stop"
            self.error |= event.get("type") == "error"

    def outcome(self):
        return "complete" if self.terminal and not self.error else "incomplete"


def event_frame(event):
    return ("data: " + json.dumps(event, ensure_ascii=False) + "\r\n\r\n").encode()


class ContractCases(unittest.TestCase):
    def test_missing_is_not_available(self):
        self.assertIsNone(remaining_ratio(total=100))
        self.assertIsNone(remaining_ratio(used=0))
        self.assertIsNone(remaining_ratio(used=0, total=0))

    def test_explicit_zero_is_preserved(self):
        self.assertEqual(remaining_ratio(remaining=0, total=100), 0)
        self.assertEqual(remaining_ratio(used=0, total=100), 1)

    def test_invalid_and_conflicting_fields_stay_unknown(self):
        for value in (True, "0", float("nan"), -1, 101):
            self.assertIsNone(remaining_ratio(used=value, total=100))
        self.assertIsNone(remaining_ratio(used=80, remaining=80, total=100))

    def test_ambiguous_count_requires_evidence(self):
        self.assertIsNone(ambiguous_remaining(count=20, total=100))
        self.assertEqual(ambiguous_remaining(count=20, total=100, percent=20), 20)
        self.assertEqual(ambiguous_remaining(count=20, total=100, percent=80), 80)
        self.assertIsNone(ambiguous_remaining(count=20, total=100, percent=50))

    def test_all_single_cut_positions_preserve_stream(self):
        events = [
            {"type": "message_start", "message": {"id": "synthetic-message"}},
            {"type": "content_block_delta", "index": 0,
             "delta": {"type": "thinking_delta", "thinking": "synthetic λ"}},
            {"type": "content_block_delta", "index": 1,
             "delta": {"type": "input_json_delta", "partial_json": '{"value":'}},
            {"type": "content_block_delta", "index": 1,
             "delta": {"type": "input_json_delta", "partial_json": '"✓"}'}},
            {"type": "future_extension", "opaque": {"synthetic": True}},
            {"type": "message_stop"},
        ]
        wire = b": synthetic heartbeat\r\n\r\n" + b"".join(map(event_frame, events))
        for cut in range(len(wire) + 1):
            observer = StreamObservation()
            observer.feed(wire[:cut])
            observer.feed(wire[cut:])
            self.assertEqual(bytes(observer.relayed), wire)
            self.assertEqual(observer.events, events)
            self.assertEqual(observer.outcome(), "complete")

    def test_eof_without_stop_is_incomplete(self):
        observer = StreamObservation()
        observer.feed(event_frame({"type": "message_start"}))
        self.assertEqual(observer.outcome(), "incomplete")

    def test_stream_error_is_not_success(self):
        observer = StreamObservation()
        observer.feed(event_frame({"type": "error", "error": {"type": "synthetic"}}))
        observer.feed(event_frame({"type": "message_stop"}))
        self.assertEqual(observer.outcome(), "incomplete")


if __name__ == "__main__":
    unittest.main()
