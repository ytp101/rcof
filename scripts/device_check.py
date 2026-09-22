#!/usr/bin/env python3
"""Short, explicit hardware check. Saves counters/logs, never microphone recordings."""
import argparse
import json
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.request

root=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser()
p.add_argument('mode',choices=['mic','speaker'])
p.add_argument('--device',required=True)
p.add_argument('--duration',type=int,default=5)
args=p.parse_args()
out=root/'runs'/('device-'+args.mode)
out.mkdir(parents=True,exist_ok=True)
with socket.socket() as s:
    s.bind(('127.0.0.1',0)); port=s.getsockname()[1]
address=f'127.0.0.1:{port}'
binary=str(root/'target/debug/rcof')
processes=[]; files=[]
def start(name,arguments):
    log=(out/(name+'.log')).open('w'); files.append(log)
    child=subprocess.Popen([binary]+arguments,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
    processes.append(child);return child
try:
    server=start('server',['server','--listen',address])
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for _ in range(50):
        try:
            with opener.open(f'http://{address}/health',timeout=1):break
        except OSError:time.sleep(0.1)
    common=['call','--server',address,'--duration',str(args.duration),'--no-stdin']
    # Only one direction uses hardware; never feed the microphone into this Mac's speakers.
    hardware=['--source','mic','--input-device',args.device] if args.mode=='mic' else ['--sink','speaker','--output-device',args.device]
    a=start('a',common+['--id','a','--stats-file',str(out/'a.json')]+hardware)
    b=start('b',common+['--id','b','--stats-file',str(out/'b.json')])
    for child in [a,b]:assert child.wait(timeout=args.duration+25)==0
    data=json.loads((out/'a.json').read_text())
    assert data['device_errors']==0,data
    print(json.dumps(data,indent=2))
finally:
    for child in reversed(processes):
        if child.poll() is None:
            child.send_signal(signal.SIGINT)
            try:child.wait(timeout=4)
            except subprocess.TimeoutExpired:child.kill();child.wait()
    for file in files:file.close()
