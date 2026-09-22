import unittest
from p2_results import describe_demo


class DemoResultsTests(unittest.TestCase):
    def test_reported_early_leave_is_normal(self):
        # A left summary does not prove who/what triggered the exit.
        summary = {'outcome': 'left', 'elapsed_seconds': 12.50850725, 'rx_frames': 607}
        ok, text = describe_demo([('a', 0, summary), ('b', 0, {**summary, 'outcome': 'peer left'})])
        self.assertTrue(ok)
        self.assertIn('processes exited successfully', text)
        self.assertIn('checks were not run', text)

    def test_leaving_before_media_is_normal(self):
        self.assertTrue(describe_demo([('a', 0, {'outcome': 'left', 'rx_frames': 0})])[0])

    def test_real_failure_is_not_hidden_by_peer_leave(self):
        self.assertFalse(describe_demo([
            ('a', 0, {'outcome': 'peer left'}),
            ('b', 0, {'outcome': 'error: camera stopped'}),
        ])[0])

    def test_crash_or_missing_result_is_not_success(self):
        self.assertFalse(describe_demo([('a', -6, {'outcome': 'left'})])[0])
        self.assertFalse(describe_demo([('a', 0, None)])[0])


if __name__ == '__main__':
    unittest.main()
