import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const source = await readFile(new URL('./rsa_timing.js', import.meta.url), 'utf8');
const method_names = ['decrypt', 'sign', 'unwrapKey', 'deriveBits', 'deriveKey'];

function create_realm(native_method = null) {
  let time = 0;
  const timers = [];
  let native_resolve;
  let native_reject;
  const native_promise = new Promise((resolve, reject) => { native_resolve = resolve; native_reject = reject; });
  const slots = new WeakMap();
  class CryptoKeyFake {
    constructor(name = 'RSA-OAEP', type = 'private') { slots.set(this, { algorithm: { name }, type }); }
  }
  Object.defineProperties(CryptoKeyFake.prototype, {
    algorithm: { get() { const slot = slots.get(this); if (!slot) throw new TypeError('Illegal invocation'); return slot.algorithm; }, configurable: true },
    type: { get() { const slot = slots.get(this); if (!slot) throw new TypeError('Illegal invocation'); return slot.type; }, configurable: true },
  });
  class Subtle {
    native(...args) { calls.push([this, args]); return native_method ? native_method.apply(this, args) : native_promise; }
    decrypt(...args) { return this.native(...args); }
    sign(...args) { return this.native(...args); }
    unwrapKey(...args) { return this.native(...args); }
    deriveBits(...args) { return this.native(...args); }
    deriveKey(...args) { return this.native(...args); }
  }
  const subtle = new Subtle();
  const calls = [];
  const native_methods = Object.fromEntries(method_names.map(name => [name, Subtle.prototype[name]]));
  const context = vm.createContext({ self: { crypto: { subtle } }, SubtleCrypto: Subtle, CryptoKey: CryptoKeyFake,
    performance: { now: () => time }, setTimeout(fn, delay) { timers.push({ fn, at: time + delay }); } });
  vm.runInContext(source, context);
  return {
    context, subtle, timers, calls, native_methods, native_promise, native_resolve, native_reject,
    set_time(value) { time = value; },
    async run_due(value) { time = value; for (;;) { const index = timers.findIndex(item => item.at <= time); if (index < 0) break; timers.splice(index, 1)[0].fn(); await Promise.resolve(); } },
    key(name = 'RSA-OAEP', type = 'private') { return new CryptoKeyFake(name, type); },
  };
}
function create_vm_native_realm(preinstall = '') {
  let time = 0; const timers = [];
  const context = vm.createContext({ performance: { now: () => time }, setTimeout(fn, delay) { timers.push({ fn, at: time + delay }); } });
  vm.runInContext(`globalThis.slots=new WeakMap(); class CryptoKey { constructor(name='RSA-OAEP',type='private'){slots.set(this,{algorithm:{name},type})} } Object.defineProperties(CryptoKey.prototype,{algorithm:{get(){const s=slots.get(this);if(!s)throw TypeError();return s.algorithm},configurable:true},type:{get(){const s=slots.get(this);if(!s)throw TypeError();return s.type},configurable:true}}); ${preinstall}`, context);
  const CryptoKeyFake = vm.runInContext('CryptoKey', context);
  const native_promise = vm.runInContext('new Promise(resolve=>globalThis.nativeResolve=resolve)', context);
  class Subtle { decrypt(...args) { this.args = args; return native_promise; } }
  const subtle = new Subtle(); context.self = { crypto: { subtle } }; context.SubtleCrypto = Subtle;
  vm.runInContext(source, context);
  return { context, subtle, native_promise, native_resolve: context.nativeResolve, timers, key() { return new CryptoKeyFake(); }, async run_due(value) { time=value; for (;;) { const i=timers.findIndex(t=>t.at<=time); if(i<0) break; timers.splice(i,1)[0].fn(); await Promise.resolve(); } } };
}
function assert_pending(promise) { let settled = false; promise.then(() => { settled = true; }, () => { settled = true; }); return Promise.resolve().then(() => assert.equal(settled, false)); }
async function flush_native() { await Promise.resolve(); await Promise.resolve(); }

test('native_promise_species_cannot_observe_private_outcomes_before_boundary', { timeout: 1000 }, async () => {
  const realm = create_vm_native_realm();
  vm.runInContext(`self.attack_observations=[]; Object.defineProperty(Promise, Symbol.species, { configurable:true, value:class AttackPromise extends Promise { constructor(executor) { super((resolve,reject)=>executor(value=>{self.attack_observations.push('resolve');resolve(value)},error=>{self.attack_observations.push('reject');reject(error)})); } } });`, realm.context);
  const result = realm.subtle.decrypt({}, realm.key()); realm.native_resolve('secret'); await flush_native();
  assert.equal(vm.runInContext('self.attack_observations.length', realm.context), 0);
  await realm.run_due(99.999); assert.equal(vm.runInContext('self.attack_observations.length', realm.context), 0);
  await realm.run_due(100); assert.equal(await result, 'secret');
});

test('private_outcomes_wait_for_settle_relative_boundaries', { timeout: 1000 }, async () => {
  for (const duration of [0, 100, 100.001, 103, 119, 124]) {
    const realm = create_realm(); const promise = realm.subtle.decrypt({}, realm.key('ECDSA'));
    realm.set_time(duration); realm.native_resolve(duration); await flush_native();
    const target = Math.max(100, Math.ceil(duration / 10) * 10);
    if (target > duration) { await assert_pending(promise); await realm.run_due(target - 0.001); await assert_pending(promise); }
    await realm.run_due(target); assert.equal(await promise, duration);
  }
  const realm = create_realm(); realm.set_time(37);
  const promise = realm.subtle.decrypt({}, realm.key()); realm.set_time(140); realm.native_resolve('late'); await flush_native();
  assert.equal(realm.timers[0].at, 147); await assert_pending(promise);
  await realm.run_due(146.999); await assert_pending(promise);
  await realm.run_due(147); assert.equal(await promise, 'late');
});
test('private_rejections_wait_for_settle_relative_boundaries', { timeout: 1000 }, async () => {
  for (const duration of [0, 103, 124]) {
    const realm = create_realm(); const error = { duration };
    const observed = realm.subtle.decrypt({}, realm.key('Ed25519')).then(() => assert.fail('Expected rejection.'), value => value);
    realm.set_time(duration); realm.native_reject(error); await flush_native();
    const target = Math.max(100, Math.ceil(duration / 10) * 10);
    await assert_pending(observed); if (target > duration) { await realm.run_due(target - 0.001); await assert_pending(observed); }
    await realm.run_due(target); assert.equal(await observed, error);
  }
});

test('all_five_methods_delay_only_for_branded_native_private_keys', { timeout: 1000 }, async () => {
  const cases = [['decrypt', [{}, null], 1], ['sign', [{}, null], 1], ['deriveBits', [{}, null, 8], 1], ['deriveKey', [{}, null, 'AES-GCM', true], 1], ['unwrapKey', ['raw', new Uint8Array([1]), null, {}, true, ['decrypt']], 2]];
  for (const [method, args, index] of cases) {
    const realm = create_realm(); args[index] = realm.key('object name', 'private');
    const promise = realm.subtle[method](...args); assert.notEqual(promise, realm.native_promise);
    assert.deepEqual(realm.calls[0][1], args); realm.native_resolve('ok'); await flush_native();
    assert.equal(realm.timers[0].at, 100); await assert_pending(promise); await realm.run_due(100); assert.equal(await promise, 'ok');
  }
});

test('algorithm_names_never_control_delay_or_get_read_by_wrapper', { timeout: 1000 }, async () => {
  for (const name of ['RSA-OAEP', 'RSA-PSS', 'RSASSA-PKCS1-v1_5', 'ECDSA', 'ECDH', 'Ed25519', 'X25519', {}, () => 'RSA-OAEP', function configurable() {}]) {
    if (typeof name === 'function') Object.defineProperty(name, 'name', { configurable: true, get() { throw Error('name getter must not run'); } });
    let reads = 0; const algorithm = {};
    const keyName = name;
    if (name && typeof name === 'object') Object.defineProperty(name, 'toString', { get() { throw Error('toString getter must not run'); } });
    Object.defineProperty(algorithm, 'name', { get() { reads++; throw Error('must not read'); } });
    let received; let realm; realm = create_realm(function (arg) { received = arg; return realm.native_promise; });
    const key = realm.key(keyName, 'private');
    Object.defineProperties(key, { algorithm: { get() { throw Error('key algorithm read'); } }, type: { get() { throw Error('key type read'); } } });
    const promise = realm.subtle.sign(algorithm, key);
    assert.equal(reads, 0); assert.equal(received, algorithm); assert.notEqual(promise, realm.native_promise); realm.native_resolve('ok'); await flush_native();
    assert.equal(reads, 0); await realm.run_due(100); assert.equal(await promise, 'ok');
  }
});

test('private_keys_ignore_callable_flipping_and_object_valued_algorithm_names', { timeout: 1000 }, async () => {
  const flipping = { reads: 0, get name() { return ++this.reads % 2 ? 'AES-GCM' : 'RSA-OAEP'; } };
  const throwing_coercion = { name: { toString() { throw Error('algorithm name coerced'); } } };
  const throwing_proxy = new Proxy({}, { get() { throw Error('algorithm get trap'); } });
  const callable = function () {};
  Object.defineProperty(callable, 'name', { get() { throw Error('callable name getter'); } });
  const variants = [callable, flipping, throwing_coercion, throwing_proxy];
  for (const algorithm of variants) {
    let realm; let received;
    realm = create_realm(function (arg) { received = arg; return realm.native_promise; });
    const key = realm.key('RSA-OAEP', 'private');
    Object.defineProperty(key, 'algorithm', { get() { throw Error('key algorithm read'); } });
    const promise = realm.subtle.sign(algorithm, key);
    assert.equal(received, algorithm); assert.notEqual(promise, realm.native_promise);
    assert.equal(flipping.reads, 0);
    realm.native_resolve('ok'); await flush_native();
    assert.equal(realm.timers[0].at, 100); await assert_pending(promise);
    await realm.run_due(100); assert.equal(await promise, 'ok');
  }
});

test('secret_public_and_invalid_keys_do_not_read_page_properties', { timeout: 1000 }, async () => {
  let algorithm_reads = 0;
  const algorithm = { get name() { algorithm_reads++; return algorithm_reads % 2 ? 'AES-GCM' : 'RSA-OAEP'; } };
  const throwing_proxy = new Proxy({}, { get() { throw Error('key get trap'); } });
  for (const realm_key of ['secret-aes', 'secret-hmac', 'public', null, undefined, 'object', 'proxy']) {
    const realm = create_realm(); let key;
    if (realm_key === 'secret-aes') key = realm.key('AES-GCM', 'secret');
    else if (realm_key === 'secret-hmac') key = realm.key('HMAC', 'secret');
    else if (realm_key === 'public') key = realm.key('RSA-OAEP', 'public');
    else if (realm_key === 'object') key = { get type() { throw Error('type getter'); }, get algorithm() { throw Error('algorithm getter'); } };
    else if (realm_key === 'proxy') key = throwing_proxy;
    else key = realm_key;
    const promise = realm.subtle.decrypt(algorithm, key);
    assert.equal(promise, realm.native_promise); assert.equal(realm.timers.length, 0);
    assert.equal(algorithm_reads, 0);
  }
});

test('captured_native_type_getter_ignores_shadow_and_later_prototype_mutation', { timeout: 1000 }, async () => {
  const realm = create_realm(); const key = realm.key('any', 'private');
  Object.defineProperty(key, 'type', { get() { throw Error('shadowed'); } });
  Object.defineProperty(vm.runInContext('CryptoKey.prototype', realm.context), 'type', { get() { throw Error('mutated'); }, configurable: true });
  const promise = realm.subtle.decrypt({}, key); assert.notEqual(promise, realm.native_promise);
  realm.native_resolve('ok'); await flush_native(); assert.equal(realm.timers.length, 1); await realm.run_due(100); assert.equal(await promise, 'ok');
});

test('native_type_getter_unavailable_or_throwing_before_install_means_no_delay', { timeout: 1000 }, async () => {
  for (const mutation of [`delete CryptoKey.prototype.type`, `Object.defineProperty(CryptoKey.prototype,'type',{get(){throw Error('getter')}})`]) {
    const realm = create_vm_native_realm(mutation); const promise = realm.subtle.decrypt({}, realm.key());
    assert.equal(promise, realm.native_promise); assert.equal(realm.timers.length, 0);
  }
});

test('wrapped_methods_are_sealed_on_prototype_and_instance', () => {
  const realm = create_realm(); const proto = Object.getPrototypeOf(realm.subtle);
  for (const name of method_names) for (const target of [proto, realm.subtle]) {
    const descriptor = Object.getOwnPropertyDescriptor(target, name); assert.equal(descriptor.configurable, false); assert.equal(descriptor.writable, false);
    assert.equal(Reflect.deleteProperty(target, name), false); assert.throws(() => Object.defineProperty(target, name, { value() {} }), TypeError);
  }
});

test('public_key_calls_preserve_promise_identity_and_synchronous_throw', () => {
  const realm = create_realm(); assert.equal(realm.subtle.decrypt({}, realm.key('RSA', 'public')), realm.native_promise);
  const error = {}; const throwing = create_realm(() => { throw error; });
  assert.throws(() => throwing.subtle.decrypt({}, throwing.key('RSA', 'public')), value => value === error);
});
test('original_arguments_receiver_and_algorithm_identity_are_forwarded_untouched', () => {
  let observed; let realm; const algorithm = { name: 'ECDSA' }; const key = {};
  realm = create_realm(function (...args) { observed = [this, args]; return realm.native_promise; });
  const result = realm.subtle.decrypt(algorithm, key);
  assert.equal(result, realm.native_promise); assert.equal(observed[0], realm.subtle); assert.equal(observed[1][0], algorithm); assert.equal(observed[1][1], key);
});

test('captured_intrinsics_and_timer_callbacks_resist_later_mutation', { timeout: 1000 }, async () => {
  const realm = create_realm(); vm.runInContext(`performance.now=()=>{throw Error()}; setTimeout=()=>{throw Error()}; Math.ceil=Math.max=()=>{throw Error()}; Promise=function(){throw Error()}; Reflect.apply=()=>{throw Error()}`, realm.context);
  const promise = realm.subtle.decrypt({}, realm.key()); realm.native_resolve('done'); await flush_native();
  const early = realm.timers.shift(); realm.set_time(50); early.fn();
  assert.equal(realm.timers[0].at, 100); await assert_pending(promise);
  await realm.run_due(99.999); await assert_pending(promise);
  await realm.run_due(100); assert.equal(await promise, 'done');
});
test('private_synchronous_throws_become_delayed_rejections', { timeout: 1000 }, async () => {
  const error = {}; let realm;
  realm = create_realm(() => { realm.set_time(140); throw error; });
  realm.set_time(37); const promise = realm.subtle.decrypt({}, realm.key());
  await assert_pending(promise); assert.equal(realm.timers[0].at, 147);
  await realm.run_due(146.999); await assert_pending(promise);
  await realm.run_due(147); await assert.rejects(promise, value => value === error);
});
