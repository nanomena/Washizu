// Version-1 migration of existing finalized records, never a game recorder.
// Public saved events repair counter definitions; matching native result
// messages supply only score metadata that those mjai files deliberately omit.
// No model imports, network requests, new games, or inferred missing outcomes.
import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const roundKey = e => `${e.bakaze}:${e.kyoku}:${e.honba}`;
const gameKey = r => r.platform === 'tenhou' ? r.match_info?.log_id : r.match_info?.game_uuid;
const csv = value => {
  if (typeof value !== 'string' || !value.length) return null;
  const parts = value.split(',');
  return parts.every(x => /^\d+$/.test(x)) ? parts.map(Number) : null;
};
export function tenhouScore(data) {
  const ten = csv(data.ten);
  if (!ten || ten.length !== 3) return null;
  const ids = data.yakuman == null ? null : csv(data.yakuman);
  if (data.yakuman != null) return ids?.length ? { yaku: ids.map(id => [id, 1]), han: 0, fu: ten[0], yakuman: true } : null;
  const pairs = csv(data.yaku);
  if (!pairs?.length || pairs.length % 2) return null;
  const yaku = [];
  for (let i = 0; i < pairs.length; i += 2) if (pairs[i + 1] > 0) yaku.push([pairs[i], pairs[i + 1]]);
  return yaku.length ? { yaku, han: yaku.reduce((n, x) => n + x[1], 0), fu: ten[0], yakuman: ten[2] >= 5 } : null;
}
function majsoulScore(hule) {
  const yaku = (hule.fans ?? []).filter(x => Number.isInteger(x.id) && x.id >= 0 && Number.isInteger(x.val) && x.val > 0).map(x => [x.id, x.val]);
  return yaku.length ? { yaku, han: hule.count ?? 0, fu: hule.fu ?? 0, yakuman: hule.yiman === true } : null;
}

// lq.ActionNewRound chang/ju/ben are uint32 fields 1/2/3 in the checked-in
// liqi.proto. Restore actions carry base64 protobuf, unlike live decoded data.
// Read only these public identity fields; protobuf's absent uint32 default is 0.
export function restoreRound(base64) {
  try {
    if (typeof base64 !== 'string' || !/^[A-Za-z0-9+/]*={0,2}$/.test(base64)) return null;
    const bytes = Buffer.from(base64, 'base64');
    let at = 0;
    const varint = () => {
      let value = 0n;
      for (let i=0;i<10;i++) { assert(at < bytes.length); const b=bytes[at++]; value |= BigInt(b&127)<<BigInt(i*7); if (!(b&128)) { assert(value <= BigInt(Number.MAX_SAFE_INTEGER)); return Number(value); } }
      throw Error('Invalid varint');
    };
    const fields = [0,0,0];
    while (at < bytes.length) {
      const key=varint(), field=Math.floor(key/8), wire=key%8; assert(field>0);
      if (field<=3) { assert.equal(wire,0); const value=varint(); assert(value<=0xffffffff); fields[field-1]=value; }
      else if (wire===0) varint();
      else if (wire===1) at+=8;
      else if (wire===2) { const length=varint(); at+=length; }
      else if (wire===5) at+=4;
      else throw Error('Unsupported protobuf wire type');
      assert(at<=bytes.length);
    }
    assert(fields[0]<4 && fields[1]<4);
    return `${['E','S','W','N'][fields[0]]}:${fields[1]+1}:${fields[2]}`;
  } catch { return null; }
}

export async function nativeScores(home, records) {
  const wanted = new Set(records.map(gameKey).filter(Boolean));
  const scores = new Map(), conflicts = new Set();
  const put = (game, round, actor, score) => {
    if (!wanted.has(game) || !round || !score) return;
    const key = `${game}|${round}|${actor}`;
    const old = scores.get(key);
    if (old && JSON.stringify(old) !== JSON.stringify(score)) { scores.delete(key); conflicts.add(key); }
    if (!conflicts.has(key)) scores.set(key, score);
  };
  const logs = path.join(home, 'logs');
  for (const run of fs.readdirSync(logs, { withFileTypes: true }).filter(x => x.isDirectory()).map(x => x.name).sort()) {
    const file = path.join(logs, run, 'inspector.jsonl');
    if (!fs.existsSync(file)) continue;
    const flows = new Map();
    const lines = readline.createInterface({ input: fs.createReadStream(file), crlfDelay: Infinity });
    for await (const line of lines) {
      if (!/authGame|syncGame|ActionNewRound|ActionHule|"method":"(?:TAIKYOKU|INIT|REINIT|AGARI)"/.test(line)) continue;
      let row; try { row = JSON.parse(line); } catch { continue; }
      if (row.kind !== 'ws_frame' || !row.parsed) continue;
      const key = row.flow_id;
      if (!key) continue;
      const flow = flows.get(key) ?? {}; flows.set(key, flow);
      const method = row.parsed.method, payload = row.parsed.args?.payload;
      if (method === '.lq.FastTest.authGame' && row.direction === 'up' && typeof payload?.game_uuid === 'string') flow.game = payload.game_uuid;
      if (row.direction !== 'down') continue;
      if (method === '.lq.FastTest.syncGame') {
        const actions=payload?.game_restore?.actions;
        if (Array.isArray(actions)) {
          const start=actions.findLast(a=>a.name==='ActionNewRound');
          flow.round=start ? restoreRound(start.data) : undefined;
        }
      }
      if (method === '.lq.ActionPrototype' && payload?.name === 'ActionNewRound') {
        const d = payload.data;
        if (Number.isInteger(d?.chang) && Number.isInteger(d?.ju) && Number.isInteger(d?.ben)) flow.round = `${['E','S','W','N'][d.chang]}:${d.ju + 1}:${d.ben}`;
      }
      if (method === '.lq.ActionPrototype' && payload?.name === 'ActionHule') for (const hule of payload.data?.hules ?? []) put(flow.game, flow.round, hule.seat, majsoulScore(hule));
      if (method === 'TAIKYOKU') flow.game = row.parsed.args?.log;
      if (method === 'INIT' || method === 'REINIT') {
        const seed = csv(row.parsed.args?.seed);
        if (seed?.length >= 2) flow.round = `${['E','S','W','N'][Math.floor(seed[0] / 4)]}:${seed[0] % 4 + 1}:${seed[1]}`;
      }
      if (method === 'AGARI') {
        const d = row.parsed.args;
        if (Number(d?.who) === 0) put(flow.game, flow.round, 'own', tenhouScore(d));
      }
    }
  }
  return { scores, conflicts };
}

export function repairRecord(record, events, metadata) {
  if (record.stats_version === 1) return { record, changed: false };
  const stats = { ...record.stats }, me = record.our_seat;
  for (const key of ['riichi_jun','agari_jun','riichi_agari_jun','fuuro_agari_jun','dama_agari_jun','houjuu','houjuu_jun','houjuu_to_oya','houjuu_point_to_oya','houjuu_point_to_ko','houjuu_point_unattributed','riichi_houjuu','fuuro_houjuu','ryukyoku','riichi_ryukyoku','nagashi_mangan']) stats[key] = 0;
  let open = false, result = false, complete = me != null, jun = 0, oya = 0, declared = false, accepted = false, calls = 0, round;
  let oldRiichiLoss = 0, oldCallLoss = 0, newRiichiLoss = 0, newCallLoss = 0, ownWins = 0, scored = 0, yakuman = 0;
  const groups = new Map(), yaku = {};
  for (const e of events) {
    if (e.type === 'start_kyoku') {
      if (open) complete = false;
      open = true; result = false; jun = 0; oya = e.oya; declared = accepted = false; calls = 0; groups.clear(); round = roundKey(e);
    }
    if (e.type === 'dahai' && e.actor === me) jun++;
    if (['chi','pon','daiminkan'].includes(e.type) && e.actor === me) calls++;
    if (e.type === 'reach' && e.actor === me) { declared = true; stats.riichi_jun += jun + 1; }
    if (e.type === 'reach_accepted' && e.actor === me) accepted = true;
    if (e.type === 'hora' || e.type === 'ryukyoku') {
      result = true;
      if (e.type === 'hora' && (e.actor === me || e.target === me) && (!Array.isArray(e.deltas) || e.deltas.length !== record.num_players)) complete = false;
    }
    if (e.type === 'ryukyoku') {
      stats.ryukyoku++; if (accepted) stats.riichi_ryukyoku++;
      if (!Array.isArray(e.deltas) && !['kyuushu','four_winds','four_riichis','three_rons','four_kans'].includes(e.reason)) complete=false;
      if (e.reason==='nagashi' && e.deltas?.[me]>0) stats.nagashi_mangan++;
    }
    if (e.type === 'hora' && e.actor === me) {
      ownWins++;
      const turn = Math.max(1, jun + Number(e.actor === e.target));
      stats.agari_jun += turn;
      stats[accepted ? 'riichi_agari_jun' : calls ? 'fuuro_agari_jun' : 'dama_agari_jun'] += turn;
      const score = metadata.get(`${gameKey(record)}|${round}|${record.platform === 'tenhou' ? 'own' : me}`);
      if (score) { scored++; if (score.yakuman) yakuman++; for (const id of new Set(score.yaku.filter(x => x[1] > 0).map(x => x[0]))) yaku[id] = (yaku[id] ?? 0) + 1; }
    }
    if (e.type === 'hora' && e.actor !== me && e.target === me && Array.isArray(e.deltas)) {
      const key = JSON.stringify(e.deltas), group = groups.get(key) ?? { delta: e.deltas, actors: new Set() };
      group.actors.add(e.actor); groups.set(key, group);
      if (declared) oldRiichiLoss += e.deltas[me]; else if (calls) oldCallLoss += e.deltas[me];
    }
    if (e.type === 'end_kyoku') {
      if (!open || !result) complete = false;
      open = false;
      if (groups.size) {
        stats.houjuu++; stats.houjuu_jun += jun;
        if ([...groups.values()].some(g => g.actors.has(oya))) stats.houjuu_to_oya++;
        let total = 0;
        for (const g of groups.values()) {
          const point = g.delta[me]; total += point;
          stats[g.actors.size > 1 ? 'houjuu_point_unattributed' : g.actors.has(oya) ? 'houjuu_point_to_oya' : 'houjuu_point_to_ko'] += point;
        }
        if (declared) { stats.riichi_houjuu++; newRiichiLoss += total; }
        else if (calls) { stats.fuuro_houjuu++; newCallLoss += total; }
        groups.clear();
      }
    }
  }
  if (open || ownWins !== (record.stats.agari ?? 0) || !(record.stats.round > 0)) complete = false;
  stats.riichi_point += newRiichiLoss - oldRiichiLoss;
  stats.fuuro_point += newCallLoss - oldCallLoss;
  let yaku_wins = null, recoveredYaku = record.yaku ?? {};
  if (scored === ownWins || Object.keys(recoveredYaku).length === 0) { recoveredYaku = yaku; yaku_wins = scored; }
  if (scored === ownWins) stats.yakuman = yakuman;
  const repaired = { ...record, stats, yaku: recoveredYaku, yaku_wins, stats_complete: complete, stats_version: 1 };
  return { record: repaired, changed: true, ownWins, recoveredWins: scored, complete };
}

export async function prepare(home) {
  const file = path.join(home,'history','index.jsonl'), original = fs.readFileSync(file);
  const records = original.toString('utf8').split(/\r?\n/).filter(Boolean).map(JSON.parse);
  const metadata = await nativeScores(home, records);
  const results = [];
  for (const record of records) {
    const events = fs.readFileSync(path.join(home,'history',record.log_path),'utf8').split(/\r?\n/).filter(Boolean).map(JSON.parse);
    results.push(repairRecord(record,events,metadata.scores));
  }
  return { schema:1, home, preparedAt:new Date().toISOString(), inputSHA256:digest(original), conflictingScores:metadata.conflicts.size, records:results.map(x=>x.record), summary:{games:records.length, changed:results.filter(x=>x.changed).length, ownWins:results.reduce((n,x)=>n+(x.record.stats.agari??0),0), recoveredWins:results.reduce((n,x)=>n+(x.record.yaku_wins??0),0), unknownCoverageGames:results.filter(x=>x.record.yaku_wins==null && x.record.stats.agari>0).length, incompleteGames:results.filter(x=>!x.record.stats_complete).map(x=>x.record.id)} };
}

export function applyProposal(proposal, backup, assertStopped) {
  assert.equal(proposal.schema,1);
  assert.equal(typeof assertStopped,'function','An offline process guard is required');
  assertStopped();
  const file=path.join(proposal.home,'history','index.jsonl'), original=fs.readFileSync(file);
  assert.equal(digest(original),proposal.inputSHA256,'History changed since prepare; prepare again');
  const old=original.toString('utf8').split(/\r?\n/).filter(Boolean).map(JSON.parse);
  assert.equal(old.length,proposal.records.length);
  const immutable=r=>Object.fromEntries(Object.entries(r).filter(([k])=>!['stats','yaku','yaku_wins','stats_complete','stats_version'].includes(k)));
  for (let i=0;i<old.length;i++) { assert.deepEqual(immutable(old[i]),immutable(proposal.records[i])); assert.equal(proposal.records[i].stats_version,1); }
  fs.mkdirSync(backup,{recursive:false});
  const copy=path.join(backup,'index.jsonl'); fs.writeFileSync(copy,original,{flag:'wx'});
  assert.equal(digest(fs.readFileSync(copy)),proposal.inputSHA256);
  const updated=Buffer.from(proposal.records.map(r=>JSON.stringify(r)).join('\n')+'\n');
  const temp=file+'.repair-v1.tmp';
  try {
    fs.writeFileSync(temp,updated,{flag:'wx'});
    assertStopped(); assert.equal(digest(fs.readFileSync(file)),proposal.inputSHA256,'History changed before commit');
    fs.renameSync(temp,file);
  } catch(error) { if(fs.existsSync(temp))fs.unlinkSync(temp); throw error; }
  const receipt={at:new Date().toISOString(),home:proposal.home,backup:copy,beforeSHA256:proposal.inputSHA256,afterSHA256:digest(updated),summary:proposal.summary};
  fs.writeFileSync(path.join(backup,'repair-receipt.json'),JSON.stringify(receipt,null,2)+'\n',{flag:'wx'});
  return receipt;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const option = name => process.argv.find(x=>x.startsWith('--'+name+'='))?.slice(name.length+3);
  const apply=option('apply');
  if(apply) {
    assert.equal(process.platform,'win32','This deployment guard is for Windows');
    const backup=option('backup'); assert(backup,'Use --apply=proposal.json --backup=NEW_DIRECTORY');
    const stopped=()=>assert.equal(Number(execFileSync('powershell.exe',['-NoProfile','-Command',"@(Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'akagi-core.exe' -or $_.Name -eq 'Washizu.exe' }).Count"],{encoding:'utf8'}).trim()),0,'Close all application processes normally before repair');
    console.log(JSON.stringify(applyProposal(JSON.parse(fs.readFileSync(apply,'utf8')),backup,stopped)));
  } else {
    const home = option('home'), out = option('out'); assert(home && out, 'Use --home=... --out=...');
    const proposal = await prepare(home);
    fs.writeFileSync(out,JSON.stringify(proposal)+'\n',{flag:'wx'});
    console.log(JSON.stringify(proposal.summary));
  }
}
