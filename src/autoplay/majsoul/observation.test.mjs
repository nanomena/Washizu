import assert from 'node:assert/strict'
import fs from 'node:fs'
import vm from 'node:vm'
import test from 'node:test'

const source=fs.readFileSync(new URL('./surface.js',import.meta.url),'utf8')
function fixture(){
  class Canvas {width=2560;height=1440;getBoundingClientRect(){return {x:100,y:50,width:1280,height:720}}}
  const canvas=new Canvas(),style={display:'block',visibility:'visible'}
  const document={getElementById:id=>id==='unity-canvas'?canvas:null,elementFromPoint:()=>canvas}
  const context=vm.createContext({HTMLCanvasElement:Canvas,document,getComputedStyle:()=>style})
  const read=point=>JSON.parse(JSON.stringify(vm.runInContext(`(${source})(${JSON.stringify(point)})`,context)))
  return {canvas,document,style,read}
}
test('actual Unity surface works with no Laya or DesktopMgr globals',()=>{
  const f=fixture();assert.deepEqual(f.read(null),{view:{viewport:{x:100,y:50,width:1280,height:720},clear:true}})
})
test('final hit point rejects an overlay and an absent or hidden Unity canvas',()=>{
  const f=fixture();let observed
  f.document.elementFromPoint=(x,y)=>{observed=[x,y];return {overlay:true}}
  assert.equal(f.read([450,650]).view.clear,false);assert.deepEqual(observed,[450,650])
  f.style.visibility='hidden';assert.deepEqual(f.read(null),{view:null})
  f.style.visibility='visible';f.canvas.width=0;assert.deepEqual(f.read(null),{view:null})
  f.document.getElementById=()=>null;assert.deepEqual(f.read(null),{view:null})
})
