import pathlib,subprocess,os,shutil
R=pathlib.Path(__file__).resolve().parent
# Keep the full security proof; throughput-only repetitions use the same fixtures.
final=os.environ.get('NN_FINAL_TRAFFIC')=='1';dedicated=os.environ.get('NN_DEDICATED_TRAFFIC')=='1';candidate='dedicated_marked' if dedicated else 'pooled_marked_final' if final else 'pooled_marked'
proof=R/('dedicated-marked-proof.json' if dedicated else 'pooled-marked-final-proof.json' if final else 'pooled-marked-proof.json');saved=proof.read_bytes()
try:
 for repeat in range(3):
  for mode in (['current_docker',candidate] if repeat%2==0 else [candidate,'current_docker']):
   subprocess.run(['python3',str(R/'pooled-proof.py')],env={**os.environ,'NN_PROOF_MODE':mode,'NN_TRAFFIC_ONLY':'1'},check=True)
   src=R/'current-docker-proof.json' if mode=='current_docker' else proof;shutil.copyfile(src,R/(('traffic-dedicated-repeat-' if dedicated else 'traffic-final-repeat-' if final else 'traffic-repeat-')+f'{repeat}-{mode}.json'));print('TRAFFIC_REPEAT',repeat,mode,flush=True)
finally:proof.write_bytes(saved)
