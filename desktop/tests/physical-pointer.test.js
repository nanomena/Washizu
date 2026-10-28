import { test } from 'node:test'
import assert from 'node:assert/strict'
import { PhysicalPointerObserver, nativeWindowIdentity } from '../src/physical-pointer.js'

const area={x:100,y:200,width:800,height:450}
const sample=(extra={})=>({point:{x:300,y:400},area,page:null,window:'1234',now:1000,...extra})
const flush=()=>new Promise(resolve=>setImmediate(resolve))
const fixture=()=>{
  /** @type {Array<Record<string,unknown>>} */ const reports=[]
  /** @type {unknown[]} */ const errors=[]
  const observer=new PhysicalPointerObserver(async p=>{reports.push(p);return false},e=>errors.push(e))
  observer.observe(sample({point:{x:10,y:20},area:null,now:900}))
  return{observer,reports,errors}
}

test('native handles use their value rather than endian byte text',()=>{
  assert.equal(nativeWindowIdentity(Buffer.from([0x34,0x12,0,0])),'1234')
  assert.equal(nativeWindowIdentity(Buffer.from([0x34,0x12,0,0,0,0,0,0])),'1234')
  assert.throws(()=>nativeWindowIdentity(Buffer.alloc(6)))
})
test('movement outside the focused game produces no ownership claim',async()=>{
  const f=fixture();f.observer.observe(sample({area:null}));await flush()
  f.observer.observe(sample({point:{x:10,y:20}}));await flush()
  assert.deepEqual(f.reports,[])
})
test('an initial stationary sample does not invent user movement when a window opens',async()=>{
  /** @type {Array<Record<string,unknown>>} */ const reports=[]
  const observer=new PhysicalPointerObserver(async p=>{reports.push(p)},e=>assert.fail(String(e)))
  observer.observe(sample());await flush()
  assert.deepEqual(reports,[{window:'1234',active:false,x:200,y:200}])
})
test('candidate ownership is resampled for overlays and never treated as an input permit',async()=>{
  const f=fixture();f.observer.observe(sample());await flush()
  f.observer.observe(sample({now:1050}));await flush()
  assert.deepEqual(f.reports,[{window:'1234',active:true},{window:'1234',active:true}])
  f.observer.observe(sample({now:1200}));await flush()
  assert.deepEqual(f.reports.at(-1),{window:'1234',active:false,x:200,y:200})
})
test('leaving after coming to rest sends an immediate release without a game position',async()=>{
  const f=fixture();f.observer.observe(sample());await flush()
  f.observer.observe(sample({now:1200}));await flush()
  f.observer.observe(sample({area:null,now:1250}));await flush()
  assert.deepEqual(f.reports.at(-1),{window:'1234',active:false})
})
test('the same observer scales an offscreen picture and a native client without platform branches',async()=>{
  for(const page of [null,{width:1600,height:900}]){
    const f=fixture();f.observer.observe(sample({page}));await flush()
    f.observer.observe(sample({page,now:1200}));await flush()
    assert.deepEqual(f.reports.at(-1),{window:'1234',active:false,x:page?400:200,y:page?400:200})
  }
})
test('only one ownership report can be pending, including a slow core response',async()=>{
  /** @type {(value:unknown)=>void} */ let resolve=()=>assert.fail('No request pending')
  /** @type {Array<Record<string,unknown>>} */ const reports=[]
  const observer=new PhysicalPointerObserver(p=>{reports.push(p);return new Promise(r=>{resolve=r})},e=>assert.fail(String(e)))
  observer.observe(sample({point:{x:10,y:20},area:null,now:900}))
  observer.observe(sample());observer.observe(sample({now:1050}));observer.observe(sample({area:null,now:1100}))
  assert.equal(reports.length,1);resolve(false);await flush()
  observer.observe(sample({area:null,now:1150}));assert.equal(reports.length,2)
  assert.deepEqual(reports.at(-1),{window:'1234',active:false});resolve(false);await flush()
})
