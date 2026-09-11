import json, os, pathlib, tempfile
import review_model_oci_executor as o
root = pathlib.Path(tempfile.mkdtemp(prefix="package-source-"))
packet = json.loads(pathlib.Path("/proof/source-packet.json").read_text())
o.materialize_source_tree(packet, root)
plan = json.loads(pathlib.Path("/proof/plan.json").read_text())
result = o.OCIExecutor(o.DEFAULT_IMAGE).execute(plan, root, evidence_ids={"E_SAMPLE", "E_CONTRACT"})
pathlib.Path("/proof-output/oci-result.json").write_text(json.dumps(result, indent=2)+"\n")
print(json.dumps({"uid":os.getuid(),"tmpdir":tempfile.gettempdir(),"source_dir":str(root),"status":result["status"],"controls_passed":result["controls_passed"],"classification":result["classification"]}))
assert result["controls_passed"] is True
assert len(result["runs"]) == 2
assert all(r["actual"]["exit"] == 0 for r in result["runs"])
