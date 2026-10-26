const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {JSDOM}=require('jsdom');
test('icon mutations scan added subtrees only and clean removed roots',async()=>{
 const dom=new JSDOM('<body><span data-icon="plus"></span></body>',{runScripts:'outside-only'}),w=dom.window;
 let scans=0,mounts=0,unmounts=0;const query=w.document.querySelectorAll.bind(w.document);w.document.querySelectorAll=(...args)=>{scans++;return query(...args)};
 w.React={createElement:()=>({})};w.createRoot=()=>{mounts++;return {render(){},unmount(){unmounts++}}};for(const name of ['IconPlus','IconSettings','IconX','IconPencil','IconTrash','IconShieldCheck','IconInfoCircle','IconFolderOpen','IconMinus'])w[name]=()=>{};
 const source=fs.readFileSync(path.resolve(__dirname,'../icons.jsx'),'utf8').split('\n').filter(line=>!line.startsWith('import ')).join('\n');w.eval(source);
 try {assert.equal(mounts,1);const initial=scans;
 const logs=w.document.createElement('div');logs.innerHTML='<span>log update</span>';w.document.body.append(logs);await new Promise(r=>setTimeout(r,0));assert.equal(scans,initial);
 const section=w.document.createElement('section');section.innerHTML='<span data-icon="plus"></span>';w.document.body.append(section);await new Promise(r=>setTimeout(r,0));assert.equal(mounts,2);assert.equal(scans,initial);
 section.remove();await new Promise(r=>setTimeout(r,0));assert.equal(unmounts,1);
 }finally{w.dispatchEvent(new w.Event('beforeunload'));dom.window.close()}
});
