"""Sharding must preserve a complete evaluation and reject mixed experiments."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT=Path(__file__).parents[1]/'scripts/merge-benchmark'

class Merge(unittest.TestCase):
    def run_merge(self, mutation=None):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            queries=root/'queries.jsonl'
            queries.write_text(''.join(json.dumps({'id':str(i),'scope':'test','query':'query'})+'\n' for i in range(2)))
            digest=hashlib.sha256(queries.read_bytes()).hexdigest()
            rows=[dict(id=str(i),scope='test',query_file_sha256=digest,shards=2,shard=i,strategy='baseline',candidates=64,model_manifest_sha256='model-a',corpus_sha256='corpus-a',hits=[],ms=1) for i in range(2)]
            if mutation:mutation(rows)
            for i,row in enumerate(rows):(root/str(i)).write_text(json.dumps(row)+'\n')
            output=root/'output.jsonl'
            result=subprocess.run([str(SCRIPT),str(queries),str(output),str(root/'0'),str(root/'1')],capture_output=True,text=True)
            return result.returncode, output.read_text() if output.exists() else None

    def test_complete_queries_are_merged_in_original_order(self):
        code,output=self.run_merge()
        self.assertEqual(code,0)
        self.assertEqual([r['id'] for r in map(json.loads,output.splitlines())],['0','1'])

    def test_mixed_models_are_rejected_before_creating_output(self):
        code,output=self.run_merge(lambda rows:rows[1].update(model_manifest_sha256='model-b'))
        self.assertNotEqual(code,0)
        self.assertIsNone(output)

    def test_mixed_corpora_are_rejected_before_creating_output(self):
        code,output=self.run_merge(lambda rows:rows[1].update(corpus_sha256='corpus-b'))
        self.assertNotEqual(code,0)
        self.assertIsNone(output)

    def test_wrong_query_membership_is_rejected(self):
        code,output=self.run_merge(lambda rows:rows[1].update(id='0'))
        self.assertNotEqual(code,0)
        self.assertIsNone(output)

if __name__=='__main__':unittest.main()
