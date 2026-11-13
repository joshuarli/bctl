import unittest

from macos_workflows import physical_footprint_sample


class PhysicalFootprintTests(unittest.TestCase):
    def test_multiple_processes_do_not_count_the_summary_twice(self):
        output = '''Python [65302]: 64-bit    Footprint: 8,274,424 B (16384 bytes per page)
Auxiliary data:
    phys_footprint: 8274424 B
    phys_footprint_peak: 8274424 B
sleep [65303]: 64-bit    Footprint: 1278192 B (16384 bytes per page)
Summary Footprint: 9519848 B
'''
        sample = physical_footprint_sample(output, {65302, 65303})
        self.assertEqual(sample['physical_bytes'], 9519848)
        self.assertEqual(len(sample['processes']), 2)

    def test_an_exited_process_cannot_make_a_partial_sample_look_complete(self):
        output = 'Python [65302]: 64-bit    Footprint: 8274424 B\nSummary Footprint: 8274424 B\n'
        with self.assertRaises(ValueError):
            physical_footprint_sample(output, {65302, 65303})


if __name__ == '__main__':
    unittest.main()
