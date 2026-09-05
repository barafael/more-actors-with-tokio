/*
 * Channel — reusable mpsc channel component.
 * Bounded buffer (cap) holding landed chars. Occupancy = landed + in-flight.
 * Sends beyond capacity are queued as blocked transactions ({ch, meta});
 * when a slot frees up, pickBlocked() hands back a random one (no FIFO bias).
 * The owner is responsible for visuals: launch a send flight around
 * launchSend()/pickBlocked() + land()/release().
 */
(function (global) {
  'use strict';

  function createChannel(capOrConfig) {
    const cap =
      typeof capOrConfig === 'object' && capOrConfig !== null ? capOrConfig.cap : capOrConfig;
    const items = []; // landed chars
    const blocked = []; // {ch, meta} in typing order
    let reserved = 0; // in-flight sends currently holding capacity

    return {
      cap,

      get length() {
        return items.length;
      },
      get items() {
        return items;
      },
      get first() {
        return items.length ? items[0] : null;
      },
      get reserved() {
        return reserved;
      },
      get occupancy() {
        return items.length + reserved;
      },
      get full() {
        return this.occupancy >= cap;
      },
      get blockedCount() {
        return blocked.length;
      },
      get blocked() {
        return blocked.slice();
      },

      // Reserve a slot for an in-flight send. Returns false if no space left.
      launchSend(v) {
        if (this.occupancy >= cap) return false;
        reserved++;
        return true;
      },

      // A reserved flight arrived; commit its char.
      land(v) {
        items.push(v);
        reserved = Math.max(0, reserved - 1);
      },

      // A reserved flight was cancelled before landing.
      release() {
        reserved = Math.max(0, reserved - 1);
      },

      // Queue a char as a blocked transaction, tagged with arbitrary meta.
      block(v, meta) {
        blocked.push({ ch: v, meta });
      },

      removeBlocked(meta) {
        for (let i = 0; i < blocked.length; i++) {
          if (blocked[i].meta === meta) {
            blocked.splice(i, 1);
            return;
          }
        }
      },

      // Hand back a random blocked transaction for delivery (random, not FIFO).
      // Does not reserve; the owner launches a flight and calls launchSend().
      pickBlocked() {
        if (this.full || blocked.length === 0) return null;
        const i = Math.floor(Math.random() * blocked.length);
        return blocked.splice(i, 1)[0];
      },

      // Consume the oldest landed char (returns null when empty).
      receive() {
        return items.length ? items.shift() : null;
      },

      reset() {
        items.length = 0;
        blocked.length = 0;
        reserved = 0;
      },
    };
  }

  const Channel = { create: createChannel };
  if (typeof module !== 'undefined' && module.exports) module.exports = Channel;
  global.Channel = global.Channel || Channel;
})(typeof window !== 'undefined' ? window : globalThis);
