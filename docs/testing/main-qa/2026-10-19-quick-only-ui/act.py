from pathlib import Path
import subprocess,sys,json,datetime,hashlib,time
root=Path(__file__).resolve().parent
args=sys.argv[1:]
entry={'timeUTC':datetime.datetime.now(datetime.timezone.utc).isoformat(),'action':args[0],'args':['[synthetic test password]' if x=='synthetic-ui-only' else x for x in args[1:]]}
if args[0] in ('click','double','drag','shot'):
    windows=json.loads(subprocess.check_output([str(root/'ui'),'windows'],text=True))
    assert windows,'Owned QA window not visible'
    window=max(windows,key=lambda w:w['kCGWindowBounds']['Width']*w['kCGWindowBounds']['Height'])
    entry['window']=window
    if args[0] in ('click','double'):
        b=window['kCGWindowBounds'];args=[args[0],str(float(args[1])+b['X']),str(float(args[2])+b['Y'])]
    elif args[0]=='drag':
        b=window['kCGWindowBounds'];args=['drag',str(float(args[1])+b['X']),str(float(args[2])+b['Y']),str(float(args[3])+b['X']),str(float(args[4])+b['Y'])]
if args[0]=='shot':
    assert args[1].replace('-','').replace('_','').isalnum()
    time.sleep(.35)
    target=root/(args[1]+'.png')
    subprocess.run(['/usr/sbin/screencapture','-x','-o','-l',str(window['kCGWindowNumber']),str(target)],check=True)
    entry['file']=target.name;entry['sha256']=hashlib.sha256(target.read_bytes()).hexdigest()
else:
    subprocess.run([str(root/'ui'),*args],check=True)
with (root/'actions.jsonl').open('a') as f:f.write(json.dumps(entry,ensure_ascii=False)+'\n')
print(json.dumps(entry,ensure_ascii=False))
