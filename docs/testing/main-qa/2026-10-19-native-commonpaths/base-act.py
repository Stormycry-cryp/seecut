from pathlib import Path
import subprocess,sys,json,datetime,hashlib,time
root=Path(__file__).resolve().parent
args=sys.argv[1:]
panel=args[0].startswith('panel-')
if panel: args[0]=args[0].removeprefix('panel-')
entry={'timeUTC':datetime.datetime.now(datetime.timezone.utc).isoformat(),'action':args[0],'args':['[typed text omitted]'] if args[0]=='text' else args[1:]}
if args[0] in ('click','double','move','drag','shot'):
    windows=json.loads(subprocess.check_output([str(root/'ui'),'windows'],text=True))
    assert windows,'Owned QA window not visible'
    window=next(w for w in windows if w['kCGWindowLayer']>0) if panel else max(windows,key=lambda w:w['kCGWindowBounds']['Width']*w['kCGWindowBounds']['Height'])
    entry['panel']=panel
    entry['window']=window
    if args[0] in ('click','double','move'):
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
