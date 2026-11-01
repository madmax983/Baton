(function () {
  var LANE = { 'host-a': 'var(--lane-a)', 'host-b': 'var(--lane-b)', 'host-c': 'var(--lane-c)' };
  var SLOTS = 16384;
  var token = (document.querySelector('meta[name="csrf-token"]') || {}).content || '';
  var busy = false, last = null;

  function el(tag, attrs, kids) {
    var e = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) {
      if (k === 'text') e.textContent = attrs[k];
      else if (k === 'style') e.setAttribute('style', attrs[k]);
      else e.setAttribute(k, attrs[k]);
    });
    (kids || []).forEach(function (c) { if (c) e.appendChild(c); });
    return e;
  }
  function secs(ms) { return (ms / 1000).toFixed(1) + 's'; }
  function n(x) { return (x || 0).toLocaleString('en-US'); }

  function post(url) {
    busy = true;
    return fetch(url, { method: 'POST', headers: { 'x-csrf-token': token }, credentials: 'same-origin' })
      .then(function (r) {
        return r.json().then(function (body) {
          if (!r.ok) throw new Error(body.detail || body.title || ('HTTP ' + r.status));
          return body;
        });
      })
      .finally(function () { busy = false; });
  }

  function lanes(s) {
    var root = document.getElementById('lanes');
    root.textContent = '';
    s.hosts.forEach(function (h) {
      var track = el('div', { class: 'track' });
      (h.view.held || []).forEach(function (r) {
        var left = r.start / SLOTS * 100, width = (r.end - r.start + 1) / SLOTS * 100;
        track.appendChild(el('div', {
          class: 'seg' + (r.serving ? '' : ' lapsed'),
          style: 'left:' + left + '%;width:' + width + '%;background:' + LANE[h.name],
          title: h.name + ' holds ' + r.range + ' at epoch ' + r.epoch + (r.serving ? '' : ' (not serving)'),
          text: r.range + ' · epoch ' + r.epoch + (r.serving ? '' : ' · lease ran out')
        }));
      });
      (h.view.waiting || []).forEach(function (w) {
        var p = w.split('-'), a = +p[0], b = +(p[1] || p[0]);
        track.appendChild(el('div', {
          class: 'seg waiting',
          style: 'left:' + (a / SLOTS * 100) + '%;width:' + ((b - a + 1) / SLOTS * 100) + '%;border-color:' + LANE[h.name],
          text: 'waiting for ' + w
        }));
      });
      root.appendChild(el('div', { class: 'lane' }, [el('div', { class: 'who', text: h.name }), track]));
    });
  }

  function hosts(s) {
    var root = document.getElementById('hosts');
    root.textContent = '';
    s.hosts.forEach(function (h) {
      var held = (h.view.held || []).map(function (r) {
        return el('div', { class: 'held' }, [document.createTextNode('holds '), el('b', { text: r.range }),
          document.createTextNode(' at epoch ' + r.epoch + (r.serving ? '' : ' — not serving'))]);
      });
      if (!held.length) held = [el('div', { class: 'held', text: h.view.reachable ? 'holds nothing' : 'not answering' })];
      (h.view.waiting || []).forEach(function (w) { held.push(el('div', { class: 'held', text: 'waiting to claim ' + w })); });
      var b = function (label, action, cls, enabled) {
        var x = el('button', { class: cls || '', 'data-post': '/hosts/' + h.name + '/' + action, text: label });
        if (!enabled) x.setAttribute('disabled', '');
        return x;
      };
      var p = h.process;
      root.appendChild(el('div', { class: 'host', style: '--lane:' + LANE[h.name] }, [
        el('div', {}, [el('span', { class: 'name', text: h.name + ' ' }), el('span', { class: 'pill ' + p, text: p })]),
        el('div', { class: 'role', text: h.role + ' · claims ' + h.slots + ' · :' + h.port + (h.pid ? ' · pid ' + h.pid : '') })
      ].concat(held).concat([el('div', { class: 'buttons' }, [
        b('Start', 'start', 'go', p === 'stopped'),
        b('kill -9', 'kill', 'danger', p !== 'stopped'),
        b('Stop cleanly', 'term', '', p === 'running'),
        b('Freeze', 'freeze', '', p === 'running'),
        b('Thaw', 'thaw', '', p === 'frozen')
      ])])));
    });
  }

  function stat(label, value, cls) {
    return el('div', { class: 'stat ' + (cls || '') }, [el('b', { text: value }), el('span', { text: label })]);
  }

  function client(s) {
    var w = s.workload, root = document.getElementById('client');
    root.textContent = '';
    root.appendChild(el('h3', { text: 'The client' }));
    root.appendChild(el('div', { class: 'hint', text: '48 runners, one write every 20 ms, each to the host that last served it; on 421 or no answer, the next host.' }));
    root.appendChild(el('div', { class: 'stats' }, [
      stat('acknowledged', n(w.acked), w.acked ? 'ok' : ''),
      stat('421 misdirected', n(w.misdirected)),
      stat('no answer', n(w.unanswered)),
      stat('no host would take it', n(w.unavailable), w.unavailable ? 'alert' : ''),
      stat('refused as stale', n(w.stale), w.stale ? 'alert' : ''),
      stat('longest outage', secs(w.longest_outage_ms), w.longest_outage_ms > 10000 ? 'alert' : '')
    ]));
    root.appendChild(w.running
      ? el('button', { 'data-post': '/client/stop', text: 'Stop the client' })
      : el('button', { class: 'go', 'data-post': '/client/start', text: 'Start the client' }));
  }

  function audit(s) {
    var a = s.audit, root = document.getElementById('audit');
    root.textContent = '';
    root.appendChild(el('h3', { text: 'The audit' }));
    if (!a) {
      root.appendChild(el('div', { class: 'hint', text: 'Read every runner back from whoever serves it now, and check every acknowledged write is there.' }));
    } else {
      root.appendChild(el('div', { class: 'verdict ' + (a.lost ? 'bad' : 'good'),
        text: a.lost ? n(a.lost) + ' acknowledged writes lost' : 'Every acknowledged write survived' }));
      root.appendChild(el('div', { class: 'stats' }, [
        stat('acknowledged', n(a.acked)), stat('present', n(a.present), 'ok'),
        stat('lost', n(a.lost), a.lost ? 'alert' : ''), stat('landed, answer lost', n(a.unacked_present))
      ]));
      if (a.lost_window_ms) {
        root.appendChild(el('div', { class: 'hint', text: 'Lost writes were acknowledged between ' + secs(a.lost_window_ms[0]) + ' and ' + secs(a.lost_window_ms[1]) + ' before this audit.' }));
      }
      if (a.lost_sample.length) {
        root.appendChild(el('div', { class: 'lost' }, a.lost_sample.map(function (l) {
          return el('div', { text: l.tenant + ' #' + l.seq + ' · ' + l.host + ' epoch ' + l.epoch + ' · at ' + secs(l.at_ms) });
        })));
      }
      if (a.unreadable.length) root.appendChild(el('div', { class: 'hint', text: 'Unreadable (no host serves them): ' + a.unreadable.join(', ') }));
    }
    root.appendChild(el('div', { class: 'buttons' }, [el('button', { 'data-post': '/audit', 'data-audit': '1', text: 'Audit every write' })]));
  }

  function timeline(s) {
    var root = document.getElementById('timeline');
    root.textContent = '';
    s.timeline.forEach(function (e) {
      root.appendChild(el('li', {}, [el('span', { class: 't', text: secs(e.at_ms) }), el('span', { class: 'k ' + e.kind, text: e.kind }), el('span', { text: e.text })]));
    });
    if (!s.timeline.length) root.appendChild(el('li', {}, [el('span', { class: 't', text: '' }), el('span', { class: 'k', text: '' }), el('span', { text: 'Nothing yet.' })]));
  }

  function render(s) { last = s; lanes(s); hosts(s); client(s); audit(s); timeline(s); }

  function load() {
    if (busy) return;
    fetch('/_state.json', { credentials: 'same-origin' }).then(function (r) { return r.json(); }).then(render).catch(function () {});
  }

  document.addEventListener('click', function (e) {
    var b = e.target.closest('[data-post]');
    if (!b || b.hasAttribute('disabled')) return;
    var c = b.getAttribute('data-confirm');
    if (c && !window.confirm(c)) return;
    b.setAttribute('disabled', '');
    var isAudit = b.hasAttribute('data-audit');
    if (isAudit) b.textContent = 'Reading every runner…';
    post(b.getAttribute('data-post')).then(function (body) {
      if (isAudit && last) { last.audit = body; render(last); } else if (body.hosts) { render(body); }
    }).catch(function (err) {
      var t = document.createElement('div'); t.className = 'err'; t.textContent = err.message;
      b.parentNode.appendChild(t); setTimeout(function () { t.remove(); }, 5000);
    }).finally(load);
  });

  setInterval(load, 500);
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', load); else load();
})();
