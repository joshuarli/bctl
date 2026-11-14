import base64
import json
import unittest

from tests.bench.macos_warm_trace import analyze_operation_trace, parse_stream


class TraceTests(unittest.TestCase):
    def test_multithread_spans_and_separate_clocks(self):
        events = [
            {'ph': 'M', 'pid': 1, 'name': 'process_name', 'args': {'name': 'Browser'}},
            {'ph': 'M', 'pid': 2, 'name': 'process_name', 'args': {'name': 'Renderer'}},
            {'ph': 'M', 'pid': 3, 'name': 'process_name', 'args': {'name': 'Viz'}},
            {'ph': 'M', 'pid': 2, 'tid': 2, 'name': 'thread_name', 'args': {'name': 'CrRendererMain'}},
            {'ph': 'M', 'pid': 3, 'tid': 3, 'name': 'thread_name', 'args': {'name': 'VizCompositorThread'}},
            {'ph': 'X', 'pid': 1, 'tid': 1, 'ts': 100, 'dur': 5000, 'tdur': 2000, 'name': 'Page.captureScreenshot'},
            {'ph': 'X', 'pid': 1, 'tid': 1, 'ts': 200, 'dur': 1000, 'tdur': 500, 'name': 'DevTools dispatch'},
            {'ph': 'B', 'pid': 2, 'tid': 2, 'ts': 300, 'tts': 10, 'name': 'EvaluateScript'},
            {'ph': 'E', 'pid': 2, 'tid': 2, 'ts': 2300, 'tts': 1010},
            {'ph': 'X', 'pid': 3, 'tid': 3, 'ts': 500, 'dur': 1000, 'tdur': 500, 'name': 'DrawFrame'},
            {'ph': 'X', 'pid': 3, 'tid': 3, 'ts': 700, 'dur': 500, 'tdur': 200, 'name': 'CopyOutputRequest'},
            {'ph': 'X', 'pid': 1, 'tid': 1, 'ts': 1500, 'dur': 500, 'tdur': 300, 'name': 'EncodePng'},
        ]
        calls = [{'method': 'Page.captureScreenshot', 'start_ns': 9000000000000, 'end_ns': 9000010000000}]
        result = analyze_operation_trace(events, calls)
        self.assertEqual(result['cdp_browser_ms'], 10)
        self.assertEqual(result['trace_window']['Renderer']['renderer_main']['union_ms'], 2)
        self.assertEqual(result['trace_window']['Renderer']['renderer_main']['cpu_ms'], 1)
        self.assertEqual(result['trace_window']['Viz']['copy_output_readback']['union_ms'], .5)
        self.assertEqual(result['cdp_command_aligned']['Browser']['image_encode']['union_ms'], .5)
        self.assertGreater(result['unattributed_ms'], 0)
        self.assertIsNone(result['unscheduled_or_idle_ms'])

    def test_stream_chunks_and_invalid_schema(self):
        raw = json.dumps({'traceEvents': [{'ph': 'M'}]}).encode()
        data, events = parse_stream([{'data': base64.b64encode(raw[:7]).decode(), 'base64Encoded': True},
                                     {'data': raw[7:].decode(), 'eof': True}])
        self.assertEqual(data, raw)
        self.assertEqual(events, [{'ph': 'M'}])
        with self.assertRaises(ValueError):
            parse_stream([{'data': '{}', 'eof': True}])

    def test_no_matching_dispatch_is_not_guessed(self):
        result = analyze_operation_trace([], [{'method': 'Runtime.evaluate', 'start_ns': 1, 'end_ns': 1000001}])
        self.assertEqual(result['cdp_command_aligned'], {})
        self.assertEqual(result['unattributed_ms'], 1)


if __name__ == '__main__':
    unittest.main()
