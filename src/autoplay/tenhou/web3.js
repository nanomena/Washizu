/* Web3 capability producer. The rewriter supplies only verified native closures.
 * Reads do not schedule work. Effects consume an identity in the same JS task.
 * `sent` means the native handler ran, not that the server acknowledged it. */
(function createWeb3Bridge(native) {
  'use strict';

  const documentId = globalThis.crypto.randomUUID();
  const nodes = new WeakMap();
  let nextNode = 0;
  const allowedPopups = new Set(['TAIKYOKU', 'AGARI', 'RYUUKYOKU', 'OWARI']);
  const u32 = value => Number.isInteger(value) && value >= 0 && value <= 0xffffffff;
  const tile = value => Number.isInteger(value) && value >= 0 && value < 136;
  const nodeId = node => {
    if (!nodes.has(node)) nodes.set(node, ++nextNode);
    return nodes.get(node);
  };
  const connected = node => !!node && node.isConnected === true;
  const visible = node => {
    if (!connected(node) || !node.getClientRects().length) return false;
    for (let current = node; current && current.nodeType === 1; current = current.parentElement) {
      const style = getComputedStyle(current);
      if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
    }
    return true;
  };
  const canonical = value => {
    if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
    if (value && typeof value === 'object') return '{' + Object.keys(value).sort().map(key => JSON.stringify(key) + ':' + canonical(value[key])).join(',') + '}';
    return JSON.stringify(value);
  };
  const copyPayload = value => {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const copy = JSON.parse(JSON.stringify(value));
    return copy && typeof copy.tag === 'string' ? copy : null;
  };
  const live = () => native.input.mode() === 1;
  const popupBlocksInput = () => {
    const popup = native.popup.read();
    // Match the client's own input gate, including a popup hidden temporarily
    // by a mouse gesture and a result being faded out.
    return connected(popup.element) && popup.element.style.display !== 'none';
  };
  const handSnapshot = () => {
    const hand = native.input.hand();
    if (!Array.isArray(hand) || !hand.length || hand.length > 14 || !hand.every(tile) || new Set(hand).size !== hand.length) return null;
    return [...hand].sort((a, b) => a - b);
  };
  const ownTarget = target => {
    if (native.input.actor() !== 0 || !native.input.isCard(target)) return false;
    const slot = native.input.targetSlot(target);
    const card = native.input.slot(slot);
    return Number.isInteger(slot) && slot >= 0 && slot < 14 && card && card.target === target && card.present && card.legal && !card.blocked;
  };

  function inputSnapshot() {
    const epoch = native.epoch();
    const hand = live() ? handSnapshot() : null;
    const unavailable = { client: 'web3', ready: false, window: null, hand: hand || [], offers: [] };
    if (!live() || !hand || !native.input.phaseReady() || native.modal() || popupBlocksInput() || !native.clock.mounted()) return { dto: unavailable, entries: [] };
    const target = native.clock.target();
    const source = native.menu.offers();
    if (!source || typeof source !== 'object') return { dto: unavailable, entries: [] };
    const discardReady = ownTarget(target);
    const responseReady = target === native.menu.responseTarget() && Object.prototype.hasOwnProperty.call(source, '2360326');
    if (!discardReady && !responseReady) return { dto: unavailable, entries: [] };
    const window = documentId + ':input:' + epoch;
    const entries = [];
    for (const [key, original] of Object.entries(source)) {
      if (!/^[0-9]+$/.test(key) || !u32(Number(key))) continue;
      const payload = copyPayload(original);
      if (!payload) continue;
      entries.push({ code: Number(key), original, payload, identity: window + ':offer:' + key + ':' + canonical(payload) });
    }
    if (native.epoch() !== epoch) return { dto: unavailable, entries: [] };
    return {
      dto: { client: 'web3', ready: true, window, hand, offers: entries.map(({ identity, payload }) => ({ identity, payload })) },
      entries, discardReady, epoch,
    };
  }

  function uiSnapshot() {
    const dto = { client: 'web3', popup: null, lobby: null };
    if (!live()) return { dto };
    const current = native.popup.read();
    const element = current.element;
    if (!connected(element)) return { dto };
    const base = documentId + ':ui:' + native.uiEpoch() + ':' + nodeId(element);
    const blocked = native.modal();
    if (current.tag !== 'LOBBY') {
      if (typeof current.tag !== 'string' || !u32(current.queued)) return { dto };
      const button = element.querySelector('button[name="ok"]');
      const enabled = allowedPopups.has(current.tag) && !current.closing && !blocked && visible(element) && visible(button) && !button.disabled;
      dto.popup = { identity: base + ':' + current.tag + ':' + current.queued + ':' + Number(current.closing) + ':' + Number(enabled),
        tag: current.tag, enabled, closing: !!current.closing, queued: current.queued };
      return { dto, element, button };
    }
    const data = native.lobby.read();
    if (!u32(data.lobby) || !Array.isArray(data.selected)) return { dto };
    const identity = base + ':lobby:' + data.lobby;
    const offers = [];
    const entries = [];
    for (const button of element.querySelectorAll('button[name="join"]')) {
      // Event/tournament buttons do not have a selected-rule slot. Never
      // reinterpret those buttons, move a tab, or choose a different rule.
      const match = /^join([0-5])$/.exec(button.id);
      if (!match) continue;
      const selected = data.selected[Number(match[1])];
      if (typeof selected !== 'string' || !/^[0-9]+$/.test(selected) || !u32(Number(selected))) continue;
      const mode = Number(selected);
      // A selected-rule button may live on a different lobby pane. P.join
      // uses that real button's id and stored rule, independent of the pane.
      const enabled = !current.closing && !blocked && visible(element) && data.loaded && !button.disabled;
      const offer = { identity: identity + ':join:' + nodeId(button) + ':' + mode, mode, enabled: !!enabled };
      offers.push(offer);
      entries.push({ offer, button });
    }
    dto.lobby = { identity, lobby: data.lobby, offers, reservationRequested: !!data.reservationRequested };
    return { dto, element, entries };
  }

  return Object.freeze({
    client: 'web3',
    readUi() { return uiSnapshot().dto; },
    pressUi(kind, identity) {
      if (kind !== 'popup' && kind !== 'join' || typeof identity !== 'string') return 'unavailable';
      if (!live()) return 'unavailable';
      const current = uiSnapshot();
      if (kind === 'popup') {
        if (!current.dto.popup || current.dto.popup.identity !== identity || !current.dto.popup.enabled) return 'changed';
        if (native.popup.read().element !== current.element || current.element.querySelector('button[name="ok"]') !== current.button) return 'changed';
        native.popup.ok();
      } else {
        const entry = current.entries && current.entries.find(entry => entry.offer.identity === identity);
        if (!entry || !entry.offer.enabled || native.popup.read().element !== current.element || !current.element.contains(entry.button)) return 'changed';
        native.lobby.join(entry.button);
      }
      return 'sent';
    },
    readInput() { return inputSnapshot().dto; },
    discard(request) {
      if (!request || typeof request.window !== 'string' || !Array.isArray(request.hand) || !request.hand.every(tile) || !tile(request.tile)) return 'unavailable';
      if (!live()) return 'unavailable';
      const current = inputSnapshot();
      if (!current.dto.ready || current.dto.window !== request.window || !current.discardReady) return 'changed';
      const expected = [...request.hand].sort((a, b) => a - b);
      if (expected.length !== current.dto.hand.length || expected.some((value, index) => value !== current.dto.hand[index])) return 'changed';
      const slot = native.input.hand().indexOf(request.tile);
      const card = native.input.slot(slot);
      if (slot < 0 || !card || !card.present || !card.legal || card.blocked || native.epoch() !== current.epoch) return 'changed';
      native.input.discard(request.tile);
      return 'sent';
    },
    choose(request) {
      if (!request || typeof request.window !== 'string' || typeof request.identity !== 'string') return 'unavailable';
      if (!live()) return 'unavailable';
      const current = inputSnapshot();
      if (!current.dto.ready || current.dto.window !== request.window) return 'changed';
      const entry = current.entries.find(entry => entry.identity === request.identity);
      if (!entry || native.menu.offers()[entry.code] !== entry.original || native.epoch() !== current.epoch) return 'changed';
      native.menu.choose(entry.code);
      return 'sent';
    },
  });
})
