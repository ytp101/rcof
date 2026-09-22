#!/usr/bin/env python3
"""Launch two native windows and their local signaling service. Ctrl-C closes all."""
import argparse, os, signal, socket, subprocess, time, urllib.request
from pathlib import Path
root=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser();p.add_argument('--seconds',type=int,default=0);p.add_argument('--screenshots',action='store_true')
p.add_argument('--binary',type=Path,default=root/'target/debug/rcof')
p.add_argument('--audio-profile',choices=['standard','efficient','minimum'],default='standard')
p.add_argument('--quality',choices=['tiny','minimal','low','standard'],default='standard')
p.add_argument('--keyframe-seconds',type=int,choices=range(1,6),default=1)
p.add_argument('--audio-complexity',type=int,choices=range(11),default=10)
p.add_argument('--no-audio-dtx',action='store_true')
args=p.parse_args()
out=root/'runs/p1-demo';out.mkdir(parents=True,exist_ok=True)
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
address=f'127.0.0.1:{port}';binary=str(args.binary.resolve())
processes=[];files=[]
def start(name,arguments):
    log=(out/(name+'.log')).open('w');files.append(log)
    child=subprocess.Popen([binary]+arguments,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
    processes.append(child);return child
try:
    server=start('server',['server','--listen',address]);http=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for _ in range(50):
        try:
            with http.open('http://'+address+'/health',timeout=1):break
        except OSError:time.sleep(.1)
    windows=[]
    for i,name in enumerate(['a','b']):
        command=['gui','--audio-complexity',str(args.audio_complexity),'--audio-profile',args.audio_profile,'--quality',args.quality,'--keyframe-seconds',str(args.keyframe_seconds),'--id',name,'--server',address,'--auto-join','--window-x',str(60+i*360),'--stats-file',str(out/(name+'.json'))]
        if args.no_audio_dtx:command+=['--no-audio-dtx']
        if args.seconds:command+=['--exit-after',str(args.seconds)]
        if args.screenshots:command+=['--screenshot',str(out/(name+'.png'))]
        windows.append(start(name,command))
    print('Two RCOF windows launched. Close both windows or press Ctrl-C here.',flush=True)
    while any(child.poll() is None for child in windows):time.sleep(.25)
    assert all(child.returncode==0 for child in windows),'window failed; inspect runs/p1-demo logs'
except KeyboardInterrupt:pass
finally:
    for child in reversed(processes):
        if child.poll() is None:
            os.killpg(child.pid,signal.SIGTERM)
            try:child.wait(timeout=5)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    for file in files:file.close()
