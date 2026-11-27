import re,sys
enc = open(sys.argv[1]).read()
m = re.search(r'IndigoHIDMessageStruct=(.*?)\]?16B24@28', enc)
# take the SimDeviceLegacyHIDClient sendWithMessage encoding line
line=[l for l in enc.splitlines() if 'sendWithMessage' in l][0]
e=line.split('enc=')[1]
e=e[e.index('{IndigoHIDMessageStruct='):]
PACK=int(sys.argv[2]) if len(sys.argv)>2 else 4
base={'I':4,'i':4,'Q':8,'q':8,'C':1,'c':1,'S':2,'s':2,'B':1,'d':8,'f':4,'?':0}
pos=0
def parse(s,i):
    # returns (type, i)
    c=s[i]
    if c in '{(':
        close='}' if c=='{' else ')'
        j=s.index('=',i); name=s[i+1:j]; i=j+1; fields=[]
        while s[i]!=close:
            t,i=parse(s,i); fields.append(t)
        return (('struct' if c=='{' else 'union'),name,fields),i+1
    if c=='[':
        j=i+1
        while s[j].isdigit(): j+=1
        n=int(s[i+1:j]); t,j=parse(s,j)
        assert s[j]==']'
        return ('array',n,t),j+1
    return ('prim',c),i+1
def size_align(t):
    k=t[0]
    if k=='prim':
        sz=base[t[1]]; return sz,min(sz,PACK) if sz else 1
    if k=='array':
        sz,al=size_align(t[2]); return sz*t[1],al
    if k=='struct':
        off=0;al=1
        for f in t[2]:
            s,a=size_align(f); off=(off+a-1)//a*a+s; al=max(al,a)
        return (off+al-1)//al*al,al
    if k=='union':
        sz=0;al=1
        for f in t[2]:
            s,a=size_align(f); sz=max(sz,s); al=max(al,a)
        return (sz+al-1)//al*al,al
def offsets(t,name,base_off=0,depth=0,out=None):
    off=0
    for idx,f in enumerate(t[2]):
        s,a=size_align(f)
        if t[0]=='struct': off=(off+a-1)//a*a
        o=base_off+(off if t[0]=='struct' else 0)
        label=f[1] if f[0] in('struct','union') else f[1] if f[0]=='prim' else 'array'
        print('  '*depth+f'+0x{o:x} size {s} {f[0]} {label}')
        if f[0] in ('struct','union') and depth<int(sys.argv[3] if len(sys.argv)>3 else 2):
            offsets(f,f[1],o,depth+1)
        if t[0]=='struct': off+=s
t,_=parse(e,0)
print('pack',PACK,'sizeof(IndigoHIDMessageStruct header+inner)=',size_align(t))
print('top-level fields of IndigoHIDMessageStruct:')
offsets(t,'IndigoHIDMessageStruct')
print()
arr=t[2][3]  # [0{...}]
el=arr[2]
print('payload element size', size_align(el)[0], 'hex', hex(size_align(el)[0]))
print('payload element offsets (relative to element start; element starts at message offset 0x20):')
offsets(el,'element',0,0)
union=[f for f in el[2] if f[0]=='union'][0]
print('union size', size_align(union)[0])
touch=[f for f in union[2] if f[1]=='_touch_event'][0]
print('touch event, offsets relative to union start (union at element +0x10 => message +0x30):')
offsets(touch,'touch',0,0)
print('button event:'); offsets([f for f in union[2] if f[1]=='_button_event'][0],'b',0,0)
