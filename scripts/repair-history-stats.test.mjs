import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { applyProposal, nativeScores, prepare, repairRecord, restoreRound, tenhouScore } from './repair-history-stats.mjs';

const record={id:'GAME',platform:'tenhou',our_seat:0,num_players:4,stats:{round:1,agari:1,riichi_point:0,fuuro_point:0},yaku:{},match_info:{log_id:'native-game'},log_path:'games/GAME.mjai.jsonl'};
const start={type:'start_kyoku',bakaze:'E',kyoku:1,honba:0,oya:1};
const end={type:'end_kyoku'};
const win={type:'hora',actor:0,target:0,deltas:[3000,-1000,-1000,-1000]};
const metadata=new Map([['native-game|E:1:0|own',{yaku:[[0,1],[1,1],[52,0]],yakuman:false}]]);

test('native metadata keeps zero-based yaku ids, ignores zero values, and does not guess malformed data',()=>{
  assert.deepEqual(tenhouScore({ten:'30,2000,0',yaku:'0,1,52,0'}).yaku,[[0,1]]);
  assert.equal(tenhouScore({ten:'30,2000,0',yaku:'1'}),null);
  assert.equal(tenhouScore({ten:'30,32000,5',yaku:'35,6,52,7'}).yakuman,true);
  assert.deepEqual(tenhouScore({ten:'0,64000,5',yakuman:'41,42'}).yaku,[[41,1],[42,1]]);
});
test('restore round uses the protobuf identity and fails closed on malformed wire',()=>{
  assert.equal(restoreRound(Buffer.from([8,1,16,2,24,3,42,1,120]).toString('base64')),'S:3:3');
  assert.equal(restoreRound(''), 'E:1:0'); // uint32 defaults on an empty protobuf message
  assert.equal(restoreRound('CgU='),null);
  assert.equal(restoreRound('!invalid'),null);
});
test('repair fixes declaration and tsumo turns while preserving identity and exact coverage',()=>{
  const r=repairRecord(record,[start,{type:'reach',actor:0},win,end],metadata).record;
  assert.equal(r.stats_complete,true);
  assert.equal(r.stats.riichi_jun,1);
  assert.equal(r.stats.agari_jun,1);
  assert.deepEqual(r.yaku,{'0':1,'1':1});
  assert.equal(r.yaku_wins,1);
  assert.deepEqual(r.match_info,record.match_info);
  assert.equal(repairRecord(r,[],new Map()).changed,false);
  assert.equal(repairRecord(record,[start,win],metadata).record.stats_complete,false);
});
test('partial metadata preserves legacy aggregates without inventing their sample count',()=>{
  const old={...record,yaku:{1:1,8:1}};
  const r=repairRecord(old,[start,win,end],new Map()).record;
  assert.deepEqual(r.yaku,old.yaku);
  assert.equal(r.yaku_wins,null);
  assert.equal(repairRecord(record,[start,win,end],new Map()).record.yaku_wins,0);
});
test('draws with no payment and double ron use hand counts and unique payments',()=>{
  const old={...record,stats:{round:1,agari:0,riichi_point:-24000,fuuro_point:0}};
  const delta=[-12000,8000,4000,0];
  const r=repairRecord(old,[start,{type:'reach',actor:0},
    {type:'hora',actor:1,target:0,deltas:delta},{type:'hora',actor:2,target:0,deltas:delta},end],new Map()).record;
  assert.equal(r.stats.houjuu,1);
  assert.equal(r.stats.houjuu_point_unattributed,-12000);
  assert.equal(r.stats.riichi_point,-12000);
  const draw=repairRecord(old,[start,{type:'ryukyoku',deltas:null,reason:'kyuushu'},end],new Map()).record;
  assert.equal(draw.stats.ryukyoku,1);
  assert.equal(draw.stats_complete,true);
});
test('native recovery matches game/round/seat and rejects contradictory result metadata',async t=>{
  const temp=fs.mkdtempSync(path.join(os.tmpdir(),'washizu-stat-test-'));
  t.after(()=>{assert(path.resolve(temp).startsWith(path.resolve(os.tmpdir())+path.sep));fs.rmSync(temp,{recursive:true});});
  fs.mkdirSync(path.join(temp,'logs','run'),{recursive:true});
  const frame=(method,args)=>({kind:'ws_frame',direction:'down',flow_id:'flow',parsed:{method,args}});
  const rows=[frame('TAIKYOKU',{log:'native-game'}),frame('INIT',{seed:'0,0'}),frame('AGARI',{who:'0',ten:'30,2000,0',yaku:'0,1'})];
  const file=path.join(temp,'logs','run','inspector.jsonl');
  fs.writeFileSync(file,rows.map(JSON.stringify).join('\n'));
  assert.equal((await nativeScores(temp,[record])).scores.size,1);
  rows.push(frame('AGARI',{who:'0',ten:'30,3900,0',yaku:'1,1'}));
  fs.writeFileSync(file,rows.map(JSON.stringify).join('\n'));
  const result=await nativeScores(temp,[record]);
  assert.equal(result.scores.size,0);assert.equal(result.conflicts.size,1);
  const soul={...record,platform:'majsoul',match_info:{game_uuid:'uuid'}};
  const auth={...frame('.lq.FastTest.authGame',{payload:{game_uuid:'uuid'}}),direction:'up'};
  const restored=frame('.lq.FastTest.syncGame',{payload:{game_restore:{actions:[{name:'ActionNewRound',data:Buffer.from([8,1,16,2,24,1]).toString('base64')}]}}});
  const hule=frame('.lq.ActionPrototype',{payload:{name:'ActionHule',data:{hules:[{seat:0,fans:[{id:2,val:1},{id:33,val:0}],count:1,fu:30,yiman:false}]}}});
  fs.writeFileSync(file,[auth,restored,hule].map(JSON.stringify).join('\n'));
  const recovered=await nativeScores(temp,[soul]);
  assert.deepEqual(recovered.scores.get('uuid|S:3:1|0').yaku,[[2,1]]);
});
test('offline application backs up bytes and rejects changed history or changed identity',async t=>{
  const temp=fs.mkdtempSync(path.join(os.tmpdir(),'washizu-stat-apply-'));
  t.after(()=>{assert(path.resolve(temp).startsWith(path.resolve(os.tmpdir())+path.sep));fs.rmSync(temp,{recursive:true});});
  fs.mkdirSync(path.join(temp,'logs'));fs.mkdirSync(path.join(temp,'history','games'),{recursive:true});
  const file=path.join(temp,'history','index.jsonl');
  const original=JSON.stringify(record)+'\n';fs.writeFileSync(file,original);
  fs.writeFileSync(path.join(temp,'history',record.log_path),[start,win,end].map(JSON.stringify).join('\n'));
  const proposal=await prepare(temp);
  const corrupt=structuredClone(proposal);corrupt.records[0].id='OTHER';
  assert.throws(()=>applyProposal(corrupt,path.join(temp,'bad-backup'),()=>{}));
  assert.throws(()=>applyProposal(proposal,path.join(temp,'running'),()=>{throw Error('running');}));
  fs.appendFileSync(file,'\n');assert.throws(()=>applyProposal(proposal,path.join(temp,'stale'),()=>{}));
  fs.writeFileSync(file,original);
  const receipt=applyProposal(proposal,path.join(temp,'backup'),()=>{});
  assert.equal(fs.readFileSync(receipt.backup,'utf8'),original);
  assert.equal(JSON.parse(fs.readFileSync(file,'utf8')).stats_version,1);
});
