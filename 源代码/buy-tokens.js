(function () {
  'use strict';

  if (!window.dsgtConfig) {
    return;
  }

  var I18N = (window.dsgtConfig && window.dsgtConfig.i18n) || {};
  function t(key, fallback) {
    return (I18N && typeof I18N[key] === 'string') ? I18N[key] : (fallback || key);
  }

  var LOCALE_MAP = { de: 'de-DE', es: 'es-ES', fr: 'fr-FR' };
  var NUM_FMT = (function () {
    var lang = (window.dsgtConfig && window.dsgtConfig.lang) || 'de';
    var locale = LOCALE_MAP[lang] || 'de-DE';
    try {
      return new Intl.NumberFormat(locale, { maximumFractionDigits: 0 });
    } catch (e) {
      return null;
    }
  })();
  function fmtNum(n) {
    var v = Number(n) || 0;
    return NUM_FMT ? NUM_FMT.format(v) : String(v);
  }

  function refreshBalanceBadges() {
    var badges = document.querySelectorAll('[data-dsgt-balance-value]');
    if (!badges.length) return;
    var bust = '?_t=' + Date.now();
    var headers = {};
    if (window.dsgtConfig && window.dsgtConfig.nonce) {
      headers['X-WP-Nonce'] = window.dsgtConfig.nonce;
    }
    fetch(window.dsgtConfig.restUrl + 'balance' + bust, { credentials: 'same-origin', cache: 'no-store', headers: headers })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (typeof data.balance === 'number') {
          badges.forEach(function (b) { b.textContent = fmtNum(data.balance); });
        }
      })
      .catch(function () {});
  }

  // Reads the AIPKit chat container's bot id (data-bot-id) so we can ask the backend
  // for the bot-specific free-quota state in the same /balance request.
  function detectBotId() {
    var el = document.querySelector('.aipkit_chat_container[data-bot-id]');
    if (!el) return 0;
    var v = parseInt(el.getAttribute('data-bot-id'), 10);
    return v > 0 ? v : 0;
  }

  // Replaces the AIPKit chat footer ("Powered by DeepSeek API") with the live token balance
  // as long as the guest holds > 0 paid tokens. If the user also has free quota left for today,
  // shows BOTH numbers via the `footer_dual` template. Restores the original footer text once
  // paid balance hits 0 (the original AIPKit message takes over again).
  function updateChatFooter() {
    var footers = document.querySelectorAll('.aipkit_chat_footer');
    if (!footers.length) return;
    var botId = detectBotId();
    var bust = '_t=' + Date.now();
    var url = window.dsgtConfig.restUrl + 'balance?' + (botId ? ('bot_id=' + botId + '&') : '') + bust;
    // X-WP-Nonce ist Pflicht damit WP den eingeloggten User erkennt — ohne wird der
    // REST-Request als anonymous behandelt, auch wenn das Auth-Cookie da ist.
    var headers = {};
    if (window.dsgtConfig && window.dsgtConfig.nonce) {
      headers['X-WP-Nonce'] = window.dsgtConfig.nonce;
    }
    fetch(url, { credentials: 'same-origin', cache: 'no-store', headers: headers })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        var bal = (typeof data.balance === 'number') ? data.balance : 0;
        var free = (data && data.free && typeof data.free.remaining === 'number') ? data.free.remaining : null;
        Array.prototype.forEach.call(footers, function (footer) {
          if (!footer.dataset.dsgtOrigHtml) {
            footer.dataset.dsgtOrigHtml = footer.innerHTML || '';
          }
          if (bal > 0 && free !== null && free > 0) {
            // Free-Quota noch verfügbar → „Gratis-Modus aktiv" anzeigen (ohne Tokenwert).
            // Sobald free auf 0 fällt, verschwindet der Hinweis und nur „Guthaben: …" bleibt.
            var dual = t('footer_dual')
              .replace('%paid%', '<strong data-dsgt-balance-value>' + escapeHtml(fmtNum(bal)) + '</strong>');
            footer.innerHTML = '<span class="dsgt-footer-balance" data-dsgt-balance>' + dual + '</span>';
          } else if (bal > 0) {
            // Paid only (kein Free-Limit, oder Free-Quota für heute aufgebraucht).
            footer.innerHTML = '<span class="dsgt-footer-balance" data-dsgt-balance>' +
              escapeHtml(t('balance_label')) +
              ' <strong data-dsgt-balance-value>' + escapeHtml(fmtNum(bal)) + '</strong></span>';
          } else if (footer.dataset.dsgtOrigHtml) {
            footer.innerHTML = footer.dataset.dsgtOrigHtml;
          }
        });
      })
      .catch(function () {});
  }

  // The chat footer may not be in the DOM yet when our script runs.
  // Retry on the next animation frames + after a short delay; also re-run on each AIPKit response.
  function tryUpdateFooterRepeatedly() {
    updateChatFooter();
    var attempts = 0;
    var interval = setInterval(function () {
      attempts++;
      if (document.querySelector('.aipkit_chat_footer') || attempts > 20) {
        updateChatFooter();
        clearInterval(interval);
      }
    }, 250);
  }

  document.addEventListener('dsgt:purchase-complete', function () {
    updateChatFooter();
  });

  function initWidget(root) {
    if (root.dataset.dsgtInit === '1') return;
    root.dataset.dsgtInit = '1';
    if (!window.dsgtConfig.mayBuy) {
      root.style.display = 'none';
      return;
    }

    var packagesEl = root.querySelector('[data-dsgt-packages]');
    var paypalContainer = root.querySelector('[data-dsgt-paypal-container]');
    var statusEl = root.querySelector('[data-dsgt-status]');
    var selected = null;
    var packages = {};

    fetch(window.dsgtConfig.restUrl + 'packages', { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        packages = data.packages || {};
        renderPackages();
        renderPayPalButtons();
      })
      .catch(function () {
        statusEl.textContent = t('packages_failed');
        statusEl.classList.add('is-error');
      });

    function renderPackages() {
      packagesEl.innerHTML = '';
      Object.keys(packages).forEach(function (key, idx) {
        var pkg = packages[key];
        var btn = document.createElement('div');
        btn.className = 'dsgt-buy__pkg' + (idx === 0 ? ' is-selected' : '');
        btn.setAttribute('role', 'radio');
        btn.setAttribute('aria-checked', idx === 0 ? 'true' : 'false');
        btn.dataset.packageKey = key;
        btn.innerHTML =
          '<span class="dsgt-buy__pkg-label">' + renderLabel(pkg.label) + '</span>' +
          '<span class="dsgt-buy__pkg-price">' + escapeHtml(pkg.price) + ' ' + escapeHtml(pkg.currency) + '</span>';
        btn.addEventListener('click', function () {
          packagesEl.querySelectorAll('.dsgt-buy__pkg').forEach(function (el) {
            el.classList.remove('is-selected');
            el.setAttribute('aria-checked', 'false');
          });
          btn.classList.add('is-selected');
          btn.setAttribute('aria-checked', 'true');
          selected = key;
        });
        packagesEl.appendChild(btn);
        if (idx === 0) selected = key;
      });
    }

    function renderPayPalButtons() {
      if (!window.paypal || typeof window.paypal.Buttons !== 'function') {
        statusEl.textContent = t('sdk_not_loaded');
        statusEl.classList.add('is-error');
        return;
      }
      window.paypal.Buttons({
        style: { layout: 'vertical', shape: 'rect', label: 'paypal' },
        createOrder: function () {
          statusEl.textContent = '';
          statusEl.classList.remove('is-error', 'is-success');
          return fetch(window.dsgtConfig.restUrl + 'create-order', {
            method: 'POST',
            credentials: 'same-origin',
            headers: {
              'Content-Type': 'application/json',
              'X-WP-Nonce': window.dsgtConfig.nonce
            },
            body: JSON.stringify({ package_key: selected })
          })
            .then(function (r) { return r.json().then(function (b) { return { ok: r.ok, body: b }; }); })
            .then(function (res) {
              if (!res.ok || !res.body.order_id) {
                throw new Error(res.body.error || 'create_failed');
              }
              return res.body.order_id;
            })
            .catch(function (err) {
              statusEl.textContent = t('order_create_failed') + err.message;
              statusEl.classList.add('is-error');
              throw err;
            });
        },
        onApprove: function (data) {
          return fetch(window.dsgtConfig.restUrl + 'capture-order', {
            method: 'POST',
            credentials: 'same-origin',
            headers: {
              'Content-Type': 'application/json',
              'X-WP-Nonce': window.dsgtConfig.nonce
            },
            body: JSON.stringify({ paypal_order_id: data.orderID })
          })
            .then(function (r) { return r.json().then(function (b) { return { ok: r.ok, body: b }; }); })
            .then(function (res) {
              if (!res.ok || !res.body.ok) {
                throw new Error(res.body.error || 'capture_failed');
              }
              statusEl.textContent = t('thanks_balance').replace('%d', fmtNum(res.body.balance));
              statusEl.classList.add('is-success');
              root.classList.add('is-purchased');
              refreshBalanceBadges();
              document.dispatchEvent(new CustomEvent('dsgt:purchase-complete', {
                detail: { balance: res.body.balance, tokens_granted: res.body.tokens_granted }
              }));
            })
            .catch(function (err) {
              statusEl.textContent = t('capture_failed') + err.message;
              statusEl.classList.add('is-error');
            });
        },
        onError: function (err) {
          statusEl.textContent = t('paypal_error') + (err && err.message ? err.message : '');
          statusEl.classList.add('is-error');
        }
      }).render(paypalContainer);
    }
  }

  function escapeHtml(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  function renderLabel(label) {
    var s = String(label == null ? '' : label);
    var m = s.match(/^(.*?)\s*(\([^)]*\))\s*$/);
    if (!m) return escapeHtml(s);
    // Nur Klammern mit „Rabatt" / „%" / „descuento" / „rabais" werden grün hervorgehoben.
    // Andere Labels (z.B. „(Einsteigerpaket)") bekommen eine neutrale Tag-Klasse.
    var inner = m[2];
    var isDiscount = /%|rabatt|descuento|rabais|discount/i.test(inner);
    var cls = isDiscount ? 'dsgt-buy__pkg-discount' : 'dsgt-buy__pkg-tag';
    return escapeHtml(m[1]) + ' <span class="' + cls + '">' + escapeHtml(inner) + '</span>';
  }

  function autoInjectOnQuotaError() {
    // AIPKit dispatches `aipkit:messageReceived` once a streamed answer is fully written
    // (after the SSE done event + cache-sse-message ajax call). At that point the usage
    // row is in the ledger, so a /balance refresh reflects the new debit. We stagger a
    // couple more refreshes for safety (some DB writes settle slightly after the event).
    var refreshAll = function () {
      updateChatFooter();
      refreshBalanceBadges();
    };
    document.addEventListener('aipkit:messageReceived', function () {
      refreshAll();
      setTimeout(refreshAll, 400);
      setTimeout(refreshAll, 1500);
    }, true);
    document.addEventListener('aipkit:messageError', function () {
      if (!window.dsgtConfig.mayBuy) return;
      var quotaEl = document.querySelector('.aipkit_chat_quota_message');
      if (quotaEl && quotaEl.textContent && quotaEl.textContent.trim() !== '') {
        ensureWidgetInjected();
      }
    });
  }

  function ensureWidgetInjected() {
    if (document.querySelector('.dsgt-quota-cta')) return;
    var chat = document.querySelector('.aipkit_chat_container');
    if (!chat) return;
    var cta = document.createElement('div');
    cta.className = 'dsgt-quota-cta';
    cta.innerHTML =
      '<p>' + escapeHtml(t('quota_inline_text')) + ' ' +
      '<a href="#" class="dsgt-buy-button dsgt-open-tokens" data-dsgt-open-tokens>' +
      escapeHtml(t('quota_inline_btn')) + '</a></p>';
    chat.parentNode.insertBefore(cta, chat.nextSibling);
  }

  // --- Lightbox / Modal ---
  var modalRoot = null;

  function buildModal() {
    var el = document.createElement('div');
    el.className = 'dsgt-modal';
    el.setAttribute('hidden', '');
    el.innerHTML =
      '<div class="dsgt-modal__overlay" data-dsgt-close></div>' +
      '<div class="dsgt-modal__inner" role="dialog" aria-modal="true" aria-labelledby="dsgt-modal-title">' +
        '<button type="button" class="dsgt-modal__close" data-dsgt-close aria-label="' + escapeHtml(t('modal_close')) + '">&times;</button>' +
        '<div class="dsgt-buy" data-dsgt-buy>' +
          '<h3 class="dsgt-buy__title" id="dsgt-modal-title">' + escapeHtml(t('modal_title')) + '</h3>' +
          '<p class="dsgt-buy__subtitle" data-dsgt-subtitle hidden></p>' +
          '<div class="dsgt-buy__packages" data-dsgt-packages>' +
            '<p class="dsgt-buy__loading">' + escapeHtml(t('packages_loading')) + '</p>' +
          '</div>' +
          '<div class="dsgt-buy__paypal" data-dsgt-paypal-container></div>' +
          '<div class="dsgt-buy__status" data-dsgt-status role="status" aria-live="polite"></div>' +
          '<div class="dsgt-restore">' +
            '<a href="#" class="dsgt-restore__toggle" data-dsgt-open-restore>' + escapeHtml(t('restore_link')) + '</a>' +
            '<div class="dsgt-restore__body" data-dsgt-restore hidden>' +
              '<label class="dsgt-restore__label">' + escapeHtml(t('restore_email')) +
                '<input type="email" data-dsgt-restore-email autocomplete="email" />' +
              '</label>' +
              '<button type="button" class="dsgt-restore__submit" data-dsgt-restore-submit>' + escapeHtml(t('restore_send')) + '</button>' +
              '<div class="dsgt-restore__status" data-dsgt-restore-status role="status" aria-live="polite"></div>' +
            '</div>' +
          '</div>' +
        '</div>' +
        '<div class="dsgt-support" data-dsgt-support hidden>' +
          '<h3 class="dsgt-buy__title">' + escapeHtml(t('support_title')) + '</h3>' +
          '<form class="dsgt-support__form" data-dsgt-support-form>' +
            '<label class="dsgt-support__label">' + escapeHtml(t('support_name')) +
              '<input type="text" name="name" required autocomplete="name" />' +
            '</label>' +
            '<label class="dsgt-support__label">' + escapeHtml(t('support_email')) +
              '<input type="email" name="email" required autocomplete="email" />' +
            '</label>' +
            '<label class="dsgt-support__label">' + escapeHtml(t('support_message')) +
              '<textarea name="message" rows="5" required></textarea>' +
            '</label>' +
            '<div class="dsgt-support__actions">' +
              '<a href="#" class="dsgt-support__back" data-dsgt-back-to-buy>' + escapeHtml(t('support_back')) + '</a>' +
              '<button type="submit" class="dsgt-support__submit">' + escapeHtml(t('support_submit')) + '</button>' +
            '</div>' +
            '<div class="dsgt-support__status" data-dsgt-support-status role="status" aria-live="polite"></div>' +
          '</form>' +
        '</div>' +
      '</div>';
    document.body.appendChild(el);
    el.addEventListener('click', function (e) {
      var closeBtn = e.target.closest('[data-dsgt-close]');
      if (closeBtn) { e.preventDefault(); closeModal(); return; }
      var openSup = e.target.closest('[data-dsgt-open-support]');
      if (openSup) { e.preventDefault(); showSupport(); return; }
      var back = e.target.closest('[data-dsgt-back-to-buy]');
      if (back) { e.preventDefault(); showBuy(); return; }
      var openRes = e.target.closest('[data-dsgt-open-restore]');
      if (openRes) { e.preventDefault(); toggleRestore(); return; }
      var resSub = e.target.closest('[data-dsgt-restore-submit]');
      if (resSub) { e.preventDefault(); handleRestoreSubmit(); return; }
    });
    var form = el.querySelector('[data-dsgt-support-form]');
    if (form) form.addEventListener('submit', handleSupportSubmit);
    return el;
  }

  // DEEPSEEK.DE: „Kauf wiederherstellen" — E-Mail-Box im Kauf-Panel aus-/einklappen.
  function toggleRestore() {
    if (!modalRoot) return;
    var body = modalRoot.querySelector('[data-dsgt-restore]');
    if (!body) return;
    body.hidden = !body.hidden;
    if (!body.hidden) {
      var inp = body.querySelector('[data-dsgt-restore-email]');
      if (inp) inp.focus();
    }
  }

  function handleRestoreSubmit() {
    if (!modalRoot) return;
    var body = modalRoot.querySelector('[data-dsgt-restore]');
    if (!body) return;
    var input = body.querySelector('[data-dsgt-restore-email]');
    var status = body.querySelector('[data-dsgt-restore-status]');
    var submit = body.querySelector('[data-dsgt-restore-submit]');
    var email = ((input && input.value) || '').trim();
    if (status) { status.className = 'dsgt-restore__status'; status.textContent = ''; }
    if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) {
      if (status) { status.textContent = t('restore_validation'); status.classList.add('is-error'); }
      return;
    }
    var orig = submit ? submit.textContent : '';
    if (submit) { submit.disabled = true; submit.textContent = t('support_sending'); }
    fetch(window.dsgtConfig.restUrl + 'resend-recovery', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-WP-Nonce': window.dsgtConfig.nonce },
      body: JSON.stringify({ email: email, lang: window.dsgtConfig.lang })
    })
      .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, body: j }; }); })
      .then(function (res) {
        if (submit) { submit.disabled = false; submit.textContent = orig; }
        if (res.ok && res.body && res.body.success) {
          if (status) { status.textContent = t('restore_sent'); status.classList.add('is-success'); }
          if (input) input.value = '';
        } else {
          if (status) { status.textContent = t('restore_error'); status.classList.add('is-error'); }
        }
      })
      .catch(function () {
        if (submit) { submit.disabled = false; submit.textContent = orig; }
        if (status) { status.textContent = t('restore_error'); status.classList.add('is-error'); }
      });
  }

  function openModal() {
    if (!modalRoot) modalRoot = buildModal();
    modalRoot.removeAttribute('hidden');
    document.body.classList.add('dsgt-modal-open');
    showBuy();
  }

  function showBuy() {
    if (!modalRoot) return;
    var buy = modalRoot.querySelector('[data-dsgt-buy]');
    var sup = modalRoot.querySelector('[data-dsgt-support]');
    if (buy) buy.hidden = false;
    if (sup) sup.hidden = true;
    if (buy) initWidget(buy);
    refreshSubtitle();
  }

  function showSupport() {
    if (!modalRoot) return;
    var buy = modalRoot.querySelector('[data-dsgt-buy]');
    var sup = modalRoot.querySelector('[data-dsgt-support]');
    if (buy) buy.hidden = true;
    if (sup) sup.hidden = false;
    var firstInput = sup && sup.querySelector('input[name="name"]');
    if (firstInput) try { firstInput.focus(); } catch (e) {}
  }

  function handleSupportSubmit(e) {
    e.preventDefault();
    var form = e.currentTarget;
    var status = form.querySelector('[data-dsgt-support-status]');
    var submit = form.querySelector('.dsgt-support__submit');
    var name = (form.elements['name'].value || '').trim();
    var email = (form.elements['email'].value || '').trim();
    var message = (form.elements['message'].value || '').trim();

    if (status) {
      status.textContent = '';
      status.className = 'dsgt-support__status';
    }
    if (!name || !email || !message || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
      if (status) {
        status.textContent = t('support_validation');
        status.classList.add('is-error');
      }
      return;
    }
    if (submit) {
      submit.disabled = true;
      submit.dataset.origLabel = submit.textContent;
      submit.textContent = t('support_sending');
    }
    fetch(window.dsgtConfig.restUrl + 'support', {
      method: 'POST',
      credentials: 'same-origin',
      headers: {
        'Content-Type': 'application/json',
        'X-WP-Nonce': window.dsgtConfig.nonce
      },
      body: JSON.stringify({ name: name, email: email, message: message })
    })
      .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, body: j }; }); })
      .then(function (res) {
        if (res.ok && res.body && res.body.ok) {
          if (status) {
            status.textContent = t('support_success');
            status.classList.add('is-success');
          }
          try { form.reset(); } catch (e) {}
        } else {
          if (status) {
            status.textContent = t('support_error');
            status.classList.add('is-error');
          }
        }
      })
      .catch(function () {
        if (status) {
          status.textContent = t('support_error');
          status.classList.add('is-error');
        }
      })
      .then(function () {
        if (submit) {
          submit.disabled = false;
          submit.textContent = submit.dataset.origLabel || t('support_submit');
        }
      });
  }

  function refreshSubtitle() {
    if (!modalRoot) return;
    var sub = modalRoot.querySelector('[data-dsgt-subtitle]');
    if (!sub) return;
    // Subtitle ist HTML mit Icon-Bullet-Items (siehe I18n::STRINGS). Vertrauenswürdige
    // Server-Strings, kein User-Input — daher innerHTML statt textContent.
    sub.innerHTML = t('modal_subtitle');
    sub.hidden = false;
  }

  function closeModal() {
    if (!modalRoot) return;
    modalRoot.remove();
    modalRoot = null;
    document.body.classList.remove('dsgt-modal-open');
  }

  document.addEventListener('click', function (e) {
    var trigger = e.target.closest('.dsgt-open-tokens, [data-dsgt-open-tokens], a[href="#dsgt-buy-tokens"], a[href$="#dsgt-buy-tokens"]');
    if (!trigger) return;
    e.preventDefault();
    openModal();
  });

  document.addEventListener('keydown', function (e) {
    if (e.key === 'Escape' && modalRoot && !modalRoot.hasAttribute('hidden')) closeModal();
  });

  // AIPKit escapes HTML inside .aipkit_chat_quota_message via textContent. Since the message
  // is already wp_kses'd on save (see DEEPSEEK.DE: patch in sanitize-settings-logic.php),
  // we can safely re-interpret the literal text as HTML so <a href="#dsgt-buy-tokens">…</a>
  // becomes a real link.
  var autoOpenedOnce = false;

  function rehydrateQuotaMessages(root) {
    var nodes = root.querySelectorAll ? root.querySelectorAll('.aipkit_chat_quota_message') : [];
    Array.prototype.forEach.call(nodes, function (el) {
      if (el.dataset.dsgtRehydrated === '1') return;
      var raw = el.textContent || '';
      if (raw.indexOf('<') === -1 || raw.indexOf('>') === -1) return;
      el.innerHTML = raw;
      el.dataset.dsgtRehydrated = '1';

      // Auto-open the lightbox if the rehydrated message links to #dsgt-buy-tokens.
      // Guard: only once per page-load (closing the modal counts as user dismissal).
      if (!autoOpenedOnce && el.querySelector('a[href$="#dsgt-buy-tokens"], a[href="#dsgt-buy-tokens"]')) {
        autoOpenedOnce = true;
        setTimeout(openModal, 600);
      }
    });
  }

  var quotaObserver = new MutationObserver(function (mutations) {
    for (var i = 0; i < mutations.length; i++) {
      var added = mutations[i].addedNodes;
      for (var j = 0; j < added.length; j++) {
        var n = added[j];
        if (n.nodeType !== 1) continue;
        if (n.classList && n.classList.contains('aipkit_chat_quota_message')) {
          rehydrateQuotaMessages(n.parentNode || document);
        } else {
          rehydrateQuotaMessages(n);
        }
      }
    }
  });
  quotaObserver.observe(document.body, { childList: true, subtree: true });

  function initAll() {
    document.querySelectorAll('[data-dsgt-buy]').forEach(initWidget);
    autoInjectOnQuotaError();
    tryUpdateFooterRepeatedly();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initAll);
  } else {
    initAll();
  }
})();
