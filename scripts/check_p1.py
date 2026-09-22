#!/usr/bin/env python3
"""P1 real-process A/V verification with no external network access."""
import argparse, json, os, signal, socket, subprocess, time, urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser()
p.add_argument('--duration',type=int,default=20)
p.add_argument('--quality',choices=['low','standard'],default='standard')
p.add_argument('--camera',action='store_true')
p.add_argument('--output',type=Path,default=ROOT/'runs/p1-check')
p.add_argument('--no-controls',action='store_true')
args=p.parse_args();args.output.mkdir(parents=True,exist_ok=True)
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
address=f'127.0.0.1:{port}';binary=str(ROOT/'target/debug/rcof')
prefix=['/usr/bin/sandbox-exec','-f',str(ROOT/'scripts/loopback-only.sb')]
processes=[];files=[]
def start(name,arguments,offline=True):
    log=(args.output/(name+'.log')).open('w');files.append(log)
    child=subprocess.Popen((prefix if offline else [])+[binary]+arguments,stdin=subprocess.PIPE,stdout=log,stderr=subprocess.STDOUT,text=True,start_new_session=True)
    processes.append(child);return child

def until(check,limit=15):
    end=time.monotonic()+limit
    while time.monotonic()<end:
        try:
            if check():return
        except (OSError,ValueError,IndexError):pass
        time.sleep(.1)
    raise AssertionError('condition timed out; inspect '+str(args.output))

def call(name,room,duration=0,source='pattern'):
    return start(name,['call','--id',name,'--room',room,'--server',address,'--duration',str(duration),'--video',source,'--quality',args.quality,'--stats-file',str(args.output/(name+'.json'))],offline=source!='camera')

def command(child,text):child.stdin.write(text+'\n');child.stdin.flush()
def snap(child,name):
    command(child,'stats');time.sleep(.25)
    return [json.loads(line) for line in (args.output/(name+'.log')).read_text().splitlines() if line.startswith('{"id"')][-1]
def connected(name):return 'connection: connected' in (args.output/(name+'.log')).read_text()
try:
    server=start('server',['server','--listen',address])
    http=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    until(lambda:http.open('http://'+address+'/health',timeout=1).status==200)
    a=call('a','av',args.duration,'camera' if args.camera else 'pattern');b=call('b','av',args.duration)
    samples=[];end=time.monotonic()+args.duration+25
    while a.poll() is None or b.poll() is None:
        assert time.monotonic()<end,'call did not stop'
        ps=subprocess.run(['ps','-axo','pid=,ppid=,%cpu=,rss=,comm='],text=True,capture_output=True).stdout
        rows=[line.split(None,4) for line in ps.splitlines()]
        owned={a.pid,b.pid}
        for _ in range(3):owned.update(int(row[0]) for row in rows if int(row[1]) in owned)
        samples.append([row for row in rows if int(row[0]) in owned])
        time.sleep(.5)
    (args.output/'resources.json').write_text(json.dumps(samples,indent=2))
    assert a.returncode==b.returncode==0,'client failed; inspect logs'
    for name in ['a','b']:
        data=json.loads((args.output/(name+'.json')).read_text())
        assert data['rx_frames']>args.duration*35,data
        assert data['video_rx_frames']>args.duration*8,data
        assert data['video_tx_frames']>args.duration*8,data
        assert data['video_frame_to_decode']['samples']>args.duration*8,data
        assert data['decode_errors']==data['device_errors']==0,data
        print(name,{key:data[key] for key in ['video_tx_frames','video_rx_frames','video_tx_kbps','transport_tx_kbps','video_frame_to_decode','av_decode_skew']},flush=True)
    print('PASS: simultaneous Opus + VP8 audio/video',flush=True)
    if not args.no_controls:
        c=call('c','controls');d=call('d','controls')
        until(lambda:connected('c') and connected('d'))
        time.sleep(2)
        assert snap(d,'d')['video_rx_frames']>5
        command(c,'camera-off');time.sleep(2)
        before=snap(d,'d');time.sleep(1);after=snap(d,'d')
        assert after['video_rx_frames']==before['video_rx_frames'],'video did not stop'
        assert after['rx_frames']>before['rx_frames']+30,'audio stopped with video'
        command(c,'camera-on');time.sleep(3)
        assert snap(d,'d')['video_rx_frames']>after['video_rx_frames']+15,'video did not resume'
        command(c,'quit');assert c.wait(timeout=8)==0;assert d.wait(timeout=8)==0
        print('PASS: video off/on keeps audio running, leave cleans up',flush=True)
finally:
    for child in reversed(processes):
        if child.poll() is None:
            try:command(child,'quit')
            except (BrokenPipeError,OSError):pass
            if child is server:child.send_signal(signal.SIGINT)
            try:child.wait(timeout=5)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    for file in files:file.close()
