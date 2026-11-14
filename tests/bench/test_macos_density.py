import tempfile
import unittest
from pathlib import Path

from tests.bench.macos_density import density_metrics, parse_footprint, shareable_files


def block(pid, physical, peak):
    return f'''Chromium [{pid}]: 64-bit    Footprint: {physical} B (16384 bytes per page)
 80000000 B           0 B           0 B         12    VM_ALLOCATE
 20000000 B           0 B     4000000 B          8    Malloc Small
 20000000 B           0 B           0 B          4    __DATA_DIRTY
        0 B    65000000 B           0 B         40    __TEXT
        0 B    30000000 B           0 B         15    mapped file
120000000 B    95000000 B     4000000 B         79    TOTAL
    phys_footprint: {physical} B
    phys_footprint_peak: {peak} B
'''


class DensityTests(unittest.TestCase):
    def test_categories_and_deduplicated_summary(self):
        output = block(1001, 120000000, 135000000) + block(1002, 120000000, 140000000) + 'Summary Footprint: 200000000 B\n'
        result = parse_footprint(output, {1001, 1002})
        self.assertEqual(result['physical_bytes'], 200000000)
        self.assertEqual(result['peak_physical_bytes'], 275000000)
        self.assertEqual(result['dirty_bytes'], 240000000)
        self.assertEqual(result['clean_bytes'], 190000000)
        self.assertEqual(result['reclaimable_bytes'], 8000000)
        self.assertEqual(result['text_clean_bytes'], 130000000)
        self.assertEqual(result['mapped_file_clean_bytes'], 60000000)
        self.assertEqual(set(result['processes']), {1001, 1002})

    def test_missing_process_and_total_fail(self):
        with self.assertRaises(ValueError):
            parse_footprint(block(1, 120000000, 135000000), {1, 2})
        with self.assertRaises(ValueError):
            parse_footprint(block(1, 120000000, 135000000).replace('TOTAL', 'other'), {1})

    def test_scaling_never_adds_guest_rss(self):
        metrics = density_metrics(3 * 2**30, 4, 2**30, 8, 2)
        self.assertEqual(metrics['per_instance_bytes'], 3 * 2**30 / 4)
        self.assertEqual(metrics['marginal_bytes'], 2 * 2**30 / 3)
        self.assertAlmostEqual(metrics['workflows_per_sec_per_gib'], 4 / 3)
        self.assertIsNone(density_metrics(2**30, 1, None, 1, 1)['marginal_bytes'])
        with self.assertRaises(ValueError):
            density_metrics(10, 2, None, 1, 1)

    def test_browser_file_sizes_are_upper_bound_not_resident_memory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'browser').write_bytes(b'\x7fELFabc')
            (root / 'resources.pak').write_bytes(b'pak')
            (root / 'icudtl.dat').write_bytes(b'icu')
            (root / 'other').write_bytes(b'ignore')
            (root / 'alias').hardlink_to(root / 'browser')
            result = shareable_files(root)
            self.assertEqual(result['file_bytes'], 13)
            self.assertEqual(len(result['files']), 3)
            self.assertIn('not observed resident', result['meaning'])


if __name__ == '__main__':
    unittest.main()
