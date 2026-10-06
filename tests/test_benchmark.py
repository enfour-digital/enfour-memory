"""Hand-calculated evidence metrics guard against inflated benchmark claims."""
import importlib.machinery
import importlib.util
import math
from pathlib import Path
import unittest

loader=importlib.machinery.SourceFileLoader('scorer',str(Path(__file__).parents[1]/'scripts/score-retrieval'))
spec=importlib.util.spec_from_loader(loader.name,loader)
scorer=importlib.util.module_from_spec(spec)
loader.exec_module(scorer)

class Metrics(unittest.TestCase):
    def test_any_hit_is_not_full_evidence_recall(self):
        m=scorer.metrics(['a','b','c'],[{'key':'wrong'},{'key':'a'}])
        self.assertEqual(m['recall@5'],1/3)
        self.assertEqual(m['hit@5'],1)
        self.assertEqual(m['all_evidence@5'],0)
        self.assertEqual(m['mrr@16'],.5)
        ideal=1+1/math.log2(3)+.5
        self.assertAlmostEqual(m['ndcg@5'],(1/math.log2(3))/ideal)

    def test_duplicates_cannot_inflate_metrics(self):
        with self.assertRaises(AssertionError):
            scorer.metrics(['a'],[{'key':'a'},{'key':'a'}])

    def test_empty_results_are_zero(self):
        self.assertTrue(all(v==0 for v in scorer.metrics(['a'],[]).values()))

if __name__=='__main__':unittest.main()
