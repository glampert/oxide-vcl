import unittest

from compare_outputs import compare, extract, normalize_instruction
from compare_corpus import diagnostic_failure


class AssemblyComparisonTests(unittest.TestCase):
    def test_status_zero_diagnostics_are_failures_not_help_or_assembly(self):
        self.assertTrue(diagnostic_failure(extract(b""), b"ERROR: failed to detect moves!\n", 0))
        self.assertFalse(diagnostic_failure(extract(b""), b"usage: vcl input\n", 0))
        self.assertFalse(diagnostic_failure(extract(b"nop[E] | nop\n"), b"ERROR: diagnostic\n", 0))
        self.assertFalse(diagnostic_failure(extract(b""), b"ERROR: invalid input\n", 255))

    def test_registers_numbers_spacing_and_case_normalize(self):
        self.assertEqual(normalize_instruction("IADDIU vi2, vi01, 0x00000001"), "iaddiu VI02,VI01,1")
        result = compare(b" ADD[E] vf2,vf1,vf1 | NOP\n", b"add[E] VF02, VF01, VF01   nop\n")
        self.assertFalse(result["raw_bytes_equal"])
        self.assertTrue(result["normalized_schedule_equal"])

    def test_original_fixed_width_pair(self):
        line = " " * 9 + "add[E] VF02,VF01,VF01".ljust(59) + "NOP".ljust(50)
        self.assertEqual(extract(line).pairs, [("add[E] VF02,VF01,VF01", "nop")])
        self.assertEqual(extract(" " + line).pairs, [("add[E] VF02,VF01,VF01", "nop")])
        wrapped = extract("DMAret *\nvifnop\nMPG 0,*\n" + line)
        self.assertEqual(wrapped.unparsed, [])
        self.assertEqual(wrapped.directives, ["DMAret *", "vifnop", "MPG 0,*"])

    def test_end_bit_loss_is_retained(self):
        result = compare(b"nop[E] | nop\n", b"nop | nop\n")
        self.assertFalse(result["normalized_schedule_equal"])
        self.assertEqual(result["expected_metrics"]["end_bit_pairs"], [0])
        self.assertEqual(result["actual_metrics"]["end_bit_pairs"], [])

    def test_labels_and_branches_are_retained(self):
        result = compare(b"loop:\nnop | b loop\n", b"other:\nnop | b other\n")
        self.assertFalse(result["normalized_schedule_equal"])
        self.assertEqual(result["expected"]["labels"], [(0, "loop")])
        self.assertEqual(normalize_instruction("b q"), "b q")
        self.assertEqual(normalize_instruction("div q,vf1X,vf2y"), "div Q,VF01x,VF02y")

    def test_stalls_and_issued_nops_have_separate_metrics(self):
        result = compare(b"mul vf2,vf1,vf1 | nop\nadd vf2,vf2,vf1 | nop ; STALL_LATENCY ?3\n",
                         b"mul vf2,vf1,vf1 | nop\nnop | nop\nnop | nop\nnop[E] | nop\nadd vf2,vf2,vf1 | nop\n")
        self.assertEqual(result["expected_metrics"]["issue_pairs"], 2)
        self.assertEqual(result["actual_metrics"]["issue_pairs"], 5)
        self.assertEqual(result["expected_metrics"]["pairs_plus_annotated_stalls"], 5)
        self.assertEqual(result["actual_metrics"]["pairs_plus_annotated_stalls"], 5)
        self.assertFalse(result["normalized_schedule_equal"])

    def test_diagnostics_are_not_empty_successful_assemblies(self):
        result = compare(b"ERROR: failure\n", b"ERROR: failure\n")
        self.assertTrue(result["raw_bytes_equal"])
        self.assertFalse(result["assembly_comparable"])
        self.assertIsNone(result["normalized_schedule_equal"])


if __name__ == "__main__":
    unittest.main()
