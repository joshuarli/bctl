#!/usr/bin/env python3
import json
import pathlib
import socket
import subprocess
import tempfile
import threading
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent


class QmpInputTests(unittest.TestCase):
    def exercise(self, reply):
        with tempfile.TemporaryDirectory(dir=ROOT / '.work' / 'tmp') as directory:
            path = str(pathlib.Path(directory) / 'qmp.sock')
            requests = []
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
                listener.bind(path)
                listener.listen(1)
                listener.settimeout(5)

                def serve():
                    connection, _ = listener.accept()
                    with connection:
                        connection.settimeout(5)
                        with connection.makefile('rwb') as stream:
                            stream.write(b'{"QMP":{}}\n')
                            stream.flush()
                            requests.append(json.loads(stream.readline()))
                            stream.write(b'{"event":"READY"}\n{"return":{}}\n')
                            stream.flush()
                            requests.append(json.loads(stream.readline()))
                            if reply is not None:
                                stream.write(json.dumps(reply).encode() + b'\n')
                                stream.flush()

                thread = threading.Thread(target=serve)
                thread.start()
                try:
                    result = subprocess.run(['python3', '-B', 'harness/qmp.py', path, 'input'],
                                            cwd=ROOT, capture_output=True, text=True, timeout=5)
                finally:
                    thread.join(5)
                    self.assertFalse(thread.is_alive(), 'QMP test server did not stop')
            self.assertEqual(requests[0], {'execute': 'qmp_capabilities'})
            self.assertEqual(len(requests), 2)
            return result, requests[1]

    def test_input_queues_complete_press_release_sequence_in_one_transaction(self):
        result, request = self.exercise({'return': {}})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(request['execute'], 'input-send-event')
        events = request['arguments']['events']
        self.assertEqual([(event['data']['key']['data'], event['data']['down']) for event in events],
                         [(code, down) for code in ['l', 'a', 'p', 'u', 't', 'a', 'ret'] for down in [True, False]])

    def test_lost_input_response_reports_uncertainty_without_resending(self):
        result, _ = self.exercise(None)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn('do not resend', result.stderr)

    def test_rejected_input_is_a_failure_instead_of_uncertain_delivery(self):
        result, _ = self.exercise({'error': {'class': 'GenericError', 'desc': 'rejected'}})
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(result.returncode, 75)
        self.assertIn('input rejected', result.stderr)


if __name__ == '__main__':
    unittest.main()
