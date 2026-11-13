import tempfile
import unittest
from pathlib import Path

from kernel_experiment import latency_percentiles, run_medians, verdict
from macos_warm_cdp import flush_profile, union_ms


class VerdictTests(unittest.TestCase):
    BASE = [90, 91, 92, 94, 95]

    def test_only_disjoint_run_medians_establish_a_change(self):
        self.assertEqual(verdict(self.BASE, [80, 82, 85, 86, 89]), 'improved')
        self.assertEqual(verdict(self.BASE, [96, 97, 99, 100, 101]), 'regressed')
        self.assertEqual(verdict(self.BASE, [80, 82, 85, 91, 99]), 'no change established')

    def test_disjoint_but_tiny_effects_are_not_changes(self):
        self.assertEqual(verdict([10.0, 10.1, 10.2, 10.3, 10.4], [9.9, 9.95, 9.96, 9.97, 9.98]), 'no change established')

    def test_fewer_than_five_runs_never_decide(self):
        self.assertEqual(verdict([90, 92, 95], [60, 61, 62]), 'too few runs')

    def test_a_missing_side_is_unmeasured_not_a_pass(self):
        self.assertEqual(verdict([], [80]), 'unmeasured')
        self.assertEqual(verdict(self.BASE, []), 'unmeasured')

    def test_run_medians_take_each_runs_median_and_skip_absent_methods(self):
        runs = [{'rows': {'FORM TOTAL': {'browser': [10, 30, 20], 'wall': [1, 2, 3]}},
                 'methods': {'DOM.resolveNode': 2.5}, 'vm_cpu_seconds': 1.5},
                {'rows': {'FORM TOTAL': {'browser': [40, 50, 60], 'wall': [4, 5, 6]}},
                 'methods': {}, 'vm_cpu_seconds': 2.0}]
        self.assertEqual(run_medians(runs, ('step', 'FORM TOTAL')), [20, 50])
        self.assertEqual(run_medians(runs, ('wall', 'FORM TOTAL')), [2, 5])
        self.assertEqual(run_medians(runs, ('method', 'DOM.resolveNode')), [2.5])
        self.assertEqual(run_medians(runs, ('cpu', 'VM CPU seconds')), [1.5, 2.0])


class ProbeParsingTests(unittest.TestCase):
    def test_union_counts_overlapping_commands_once_and_clips_to_the_step(self):
        ms = 1_000_000
        intervals = [(0, 10 * ms), (5 * ms, 15 * ms), (20 * ms, 30 * ms), (40 * ms, 60 * ms)]
        self.assertEqual(union_ms(intervals, 0, 50 * ms), 15 + 10 + 10)

    def test_flush_profile_reports_deltas_and_none_without_a_profile_kernel(self):
        console = ('boot\nhydrogen-flush monotonic_us=1 posts=10 user_loads=100\n'
                   'noise\nhydrogen-flush monotonic_us=3000001 posts=25 user_loads=400\n')
        self.assertEqual(flush_profile(console), {'monotonic_us': 3000000, 'posts': 15, 'user_loads': 300})
        self.assertIsNone(flush_profile('boot only\n'))

    def test_latency_percentiles_come_from_each_runs_serial_log(self):
        with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parents[2] / '.work/tmp') as tmp:
            runs = Path(tmp)
            for smp, p50 in ((1, '1.8'), (2, '6.0')):
                run = runs / f'20261113T000000Z-wakeup_latency-smp{smp}-1'
                run.mkdir()
                (run / 'serial.log').write_text(
                    f'MINI_CHROMIUM_TEST name=wakeup_latency status=PASS detail=p50_us={p50},p90_us=7.3,p99_us=9.0\n')
            (runs / '20261113T000000Z-probe-smp2-1').mkdir()
            self.assertEqual(latency_percentiles(runs), {
                'smp1': {'p50_us': 1.8, 'p90_us': 7.3, 'p99_us': 9.0},
                'smp2': {'p50_us': 6.0, 'p90_us': 7.3, 'p99_us': 9.0}})


if __name__ == '__main__':
    unittest.main()
