import base64
import unittest
import zlib
from unittest.mock import patch

import kitty_probe as probe


class ProtocolTests(unittest.TestCase):
    def test_diagnostics_reads_inherited_option(self):
        with patch.dict(probe.os.environ, {"TMUX": "private,1,0", "TMUX_PANE": "%1"}), \
                patch.object(probe.shutil, "which", return_value="tmux"), \
                patch.object(probe, "tmux_read", side_effect=["tmux 3.7", "on"]) as read, \
                patch("builtins.print"):
            self.assertTrue(probe.diagnostics())
        self.assertEqual(read.call_args.args,
                         ("show-options", "-A", "-v", "-t", "%1", "allow-passthrough"))

    def test_updates_never_rewrite_placeholders_and_panel_timing(self):
        now = [0.0]
        terminal = unittest.mock.Mock(wire_bytes=0)
        layout = unittest.mock.Mock(w=2, h=2)
        layout.cards = [(42 + i, 2, 1) for i in range(4)]
        layout.placeholders.return_value = b"initial placeholders"
        layout.status.return_value = b"status"
        times = []
        layout.panel.side_effect = lambda: times.append(("show", now[0])) or b"panel"
        layout.restore_panel.side_effect = lambda: times.append(("restore", now[0])) or b"restore"
        def sleep(seconds):
            now[0] += seconds
        with patch.object(probe.time, "monotonic", side_effect=lambda: now[0]), \
                patch.object(probe.time, "sleep", side_effect=sleep), \
                patch.object(probe, "draw_frame", return_value=b"abc") as draw:
            result = probe.animate(terminal, layout, False)
        layout.placeholders.assert_called_once_with()
        self.assertEqual(times, [("show", 10.0), ("restore", 18.0)])
        self.assertEqual(draw.call_count, 240)
        self.assertEqual(terminal.graphics.call_count, 240)
        self.assertIn("60 ticks / 30.00s", result[0])

    def test_passthrough_exact_bytes(self):
        inner = b"\x1b_Ga=q;AAAA\x1b\\"
        self.assertEqual(probe.tmux_wrap(inner),
                         b"\x1bPtmux;\x1b\x1b_Ga=q;AAAA\x1b\x1b\\\x1b\\")

    def test_query_exact_bytes(self):
        self.assertEqual(probe.query_command(42),
                         b"\x1b_Ga=q,i=42,t=d,f=32,s=1,v=1;AAAA/w==\x1b\\")

    def test_placeholder_rows_exact_bytes(self):
        prefix = b"\x1b[0m\x1b[58;2;0;0;0m\x1b[38;2;1;2;3m"
        # Literal UTF-8 bytes: U+10EEEE, row 0/1, column 0/1.
        self.assertEqual(probe.placeholder_row(0x010203, 0, 0, 2), prefix +
                         b"\xf4\x8e\xbb\xae\xcc\x85\xcc\x85"
                         b"\xf4\x8e\xbb\xae\xcc\x85\xcc\x8d\x1b[0m")
        self.assertEqual(probe.placeholder_row(0x010203, 1, 0, 2), prefix +
                         b"\xf4\x8e\xbb\xae\xcc\x8d\xcc\x85"
                         b"\xf4\x8e\xbb\xae\xcc\x8d\xcc\x8d\x1b[0m")

    def test_fragment_preserves_original_column(self):
        self.assertEqual(probe.placeholder_row(42, 2, 3, 1),
                         b"\x1b[0m\x1b[58;2;0;0;0m\x1b[38;2;0;0;42m"
                         b"\xf4\x8e\xbb\xae\xcc\x8e\xcc\x90\x1b[0m")

    def test_chunk_headers_exact_bytes(self):
        chunks = list(probe.image_chunks(b"abcdef", 42, 2, 2, width=1, height=1, chunk_size=4))
        self.assertEqual(chunks, [
            b"\x1b_Ga=T,t=d,f=32,o=z,s=1,v=1,i=42,U=1,c=2,r=2,q=2,m=1;YWJj\x1b\\",
            b"\x1b_Gm=0,q=2;ZGVm\x1b\\"])
        self.assertEqual(list(probe.image_chunks(b"abc", 42, 2, 2, width=1, height=1)), [
            b"\x1b_Ga=T,t=d,f=32,o=z,s=1,v=1,i=42,U=1,c=2,r=2,q=2,m=0;YWJj\x1b\\"])

    def test_compressed_rgba_round_trip_and_chunk_limit(self):
        raw = bytes(range(256)) * 200
        chunks = list(probe.image_chunks(zlib.compress(raw, 0), 42, 20, 10))
        payloads = [c.split(b";", 1)[1][:-2] for c in chunks]
        self.assertGreater(len(chunks), 2)
        self.assertTrue(all(len(p) <= 4096 and len(p) % 4 == 0 for p in payloads))
        self.assertEqual(zlib.decompress(base64.b64decode(b"".join(payloads))), raw)
        self.assertTrue(all(b"m=1" in c for c in chunks[:-1]))
        self.assertIn(b"m=0", chunks[-1])

    def test_invalid_coordinates_and_chunk_sizes(self):
        for args in [(0, 0, 0, 1), (1 << 24, 0, 0, 1), (42, 64, 0, 1), (42, 0, 63, 2)]:
            with self.assertRaises(ValueError):
                probe.placeholder_row(*args)
        for size in (0, 3, 4100):
            with self.assertRaises(ValueError):
                list(probe.image_chunks(b"abc", 42, 2, 2, chunk_size=size))

    def test_cursor_sync_delete_exact_bytes(self):
        self.assertEqual(probe.cursor(2, 3), b"\x1b[2;3H")
        self.assertEqual(probe.sync_output(b"hello"), b"\x1b[?2026hhello\x1b[?2026l")
        self.assertEqual(probe.graphics_command(dict(a="d", d="I", i=42, q=2)),
                         b"\x1b_Ga=d,d=I,i=42,q=2;\x1b\\")

    def test_panel_restoration_only_intersecting_cells(self):
        terminal = unittest.mock.Mock()
        terminal.size.return_value = (40, 120, 10, 20)
        layout = probe.Layout(terminal, [41, 42, 43, 44])
        region = (10, 20, 1, 2)
        self.assertEqual(layout.placeholders(region), probe.cursor(10, 20) +
                         probe.placeholder_row(41, 8, 19, 2))
        self.assertNotIn(probe.PLACEHOLDER.encode(), layout.panel())

    def test_query_accepts_split_reply_and_restores_tty(self):
        terminal = probe.Terminal(123)
        with patch.object(terminal, "graphics") as send, patch.object(probe, "cbreak") as mode, \
                patch.object(probe.select, "select", return_value=([123], [], [])), \
                patch.object(probe.os, "read", side_effect=[b"\x1b_Gi=16777215;", b"OK\x1b\\"]):
            reply, supported = terminal.query(1)
        self.assertTrue(supported)
        self.assertEqual(reply, b"\x1b_Gi=16777215;OK\x1b\\")
        send.assert_called_once_with(probe.query_command())
        mode.return_value.__exit__.assert_called_once()


if __name__ == "__main__":
    unittest.main()
