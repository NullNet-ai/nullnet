import json,pathlib,re,time,sys
label=sys.argv[1]; output=pathlib.Path('/tmp/nn-layer1-'+label+'.population.json');stop=output.with_suffix('.stop');rows=[]
while not stop.exists():
 names=[p.name for p in pathlib.Path('/sys/class/net').iterdir()]; owned=[n for n in names if re.match(r'^(br_\d+_[sc]|nnv_\d+_[sc]|vxlan-ns_\d+_[sc]|ns_\d+_[sc]-(out|o)|veth-\d+-[sc]|macsec-\d+-?[sc])$',n)]
 rows.append({'unix':time.time(),'owned_root_links':len(owned),'root_links':len(names)});time.sleep(.25)
output.write_text(json.dumps(rows)+'\n')
