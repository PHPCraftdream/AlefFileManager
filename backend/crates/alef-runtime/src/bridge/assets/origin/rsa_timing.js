(function () {
    "use strict";
    var NativePromise = Promise;
    var promiseConstructor = {};
    Object.defineProperty(promiseConstructor, Symbol.species, { value: NativePromise });
    var then = NativePromise.prototype.then;
    var apply = Reflect.apply;
    var now = performance.now.bind(performance);
    var timer = setTimeout.bind(self);
    var ceil = Math.ceil;
    var max = Math.max;
    var define = Object.defineProperty;
    var descriptor = Object.getOwnPropertyDescriptor;
    var prototype = Object.getPrototypeOf;
    var floor = 100;
    var step = 10;
    var subtle = self.crypto && self.crypto.subtle;
    if (!subtle) return;
    var proto = typeof SubtleCrypto === "function" ? SubtleCrypto.prototype : prototype(subtle);
    var keyProto = typeof CryptoKey === "function" ? CryptoKey.prototype : null;
    var keyType = keyProto && descriptor(keyProto, "type");
    function isPrivateKey(key) {
        if (!keyType || typeof keyType.get !== "function") return false;
        try { return apply(keyType.get, key, []) === "private"; } catch (_) { return false; }
    }
    ["decrypt", "sign", "unwrapKey", "deriveBits", "deriveKey"].forEach(function (name) {
        var method = descriptor(proto, name);
        if (!method || typeof method.value !== "function") return;
        var native = method.value;
        var wrapped = function () {
            var keyIndex = name === "unwrapKey" ? 2 : 1;
            var delay = isPrivateKey(arguments[keyIndex]);
            var start = now();
            var result;
            try { result = apply(native, this, arguments); }
            catch (error) {
                if (!delay) throw error;
                return new NativePromise(function (resolve, reject) {
                    var elapsed = now() - start;
                    var target = start + max(floor, ceil(elapsed / step) * step);
                    function settle() { var remaining = target - now(); if (remaining > 0) { timer(settle, remaining); return; } reject(error); }
                    settle();
                });
            }
            if (!delay) return result;
            define(result, "constructor", { value: promiseConstructor });
            return new NativePromise(function (resolve, reject) {
                function settle(callback, value) {
                    var elapsed = now() - start;
                    var target = start + max(floor, ceil(elapsed / step) * step);
                    function check() { var remaining = target - now(); if (remaining > 0) { timer(check, remaining); return; } callback(value); }
                    check();
                }
                apply(then, result, [function (value) { settle(resolve, value); }, function (error) { settle(reject, error); }]);
            });
        };
        define(proto, name, { value: wrapped, writable: false, enumerable: method.enumerable, configurable: false });
        try { define(subtle, name, { value: wrapped, writable: false, enumerable: method.enumerable, configurable: false }); } catch (_) {}
    });
})();
