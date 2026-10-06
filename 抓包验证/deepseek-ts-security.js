<script id="deepseek-ts-js">
    (function () {
        var SITEKEY = '0x4AAAAAADlLZ3ljqZP6cQwq';
        var AJAXURL = 'https://deepseek.es/wp-admin/admin-ajax.php';
        var ready   = false;          // Turnstile-API geladen?
        var instances = [];           // {container, wrap, inputArea}
        var hooked  = false;          // Netzwerk-Interception nur einmal einrichten
        var MSG_CHECK = '\u{1F512} Sicherheitsprüfung läuft…';
        var MSG_OK    = '✓ Bestätigt';
        var MSG_ERR   = '⚠ Prüfung wird wiederholt…';

        function lockSend(scope, lock) {
            var btn = scope.querySelector('.aipkit_send_btn');
            if (btn) {
                btn.classList.toggle('deepseek-ts-locked', lock);
                btn.disabled = lock;
                if (lock) { btn.setAttribute('aria-disabled', 'true'); }
                else      { btn.removeAttribute('aria-disabled'); }
            }
        }

        function setStatus(wrap, text, cls) {
            var s = wrap.querySelector('.deepseek-ts-status');
            if (!s) return;
            s.className = 'deepseek-ts-status' + (cls ? ' ' + cls : '');
            s.innerHTML = (cls === 'ok' || cls === 'err' ? '' :
                '<span class="deepseek-ts-spin"></span>') +
                '<span>' + text + '</span>';
        }

        function dismiss(container) {
            var wrap = container.querySelector('.deepseek-ts-wrap');
            if (!wrap || wrap.dataset.gone === '1') return;
            wrap.dataset.gone = '1';
            wrap.style.transition = 'opacity .4s ease';
            wrap.style.opacity = '0';
            // NICHT entfernen: ausblenden, aber im DOM lassen, damit Turnstile den
            // Token im Hintergrund weiter erneuert und der Cookie nie abläuft.
            setTimeout(function () { wrap.classList.add('deepseek-ts-vh'); }, 450);
        }

        // Lesbarer Marker-Cookie vorhanden? -> Nutzer ist (noch) verifiziert.
        function dsTsMarker() {
            return /(?:^|;\s*)dsts_ok=1(?:;|$)/.test(document.cookie || '');
        }

        function process(container) {
            if (!ready || container.dataset.deepseekTs) return;

            var inputArea = container.querySelector('.aipkit_chat_input');
            if (!inputArea) return; // Chat noch nicht fertig gerendert → später erneut versuchen
            container.dataset.deepseekTs = '1';
            // Bereits verifiziert (Marker-Cookie)? -> passiv: kein Lock, Widget unsichtbar
            // (nur stille Cookie-Erneuerung im Hintergrund). Sonst normal sperren+prüfen.
            var alreadyOk = dsTsMarker();

            // UI-Block UNTER dem Footer ("powered by") einsetzen; sonst über dem Eingabefeld
            var wrap = document.createElement('div');
            wrap.className = 'deepseek-ts-wrap';
            wrap.innerHTML =
                '<span class="deepseek-ts-status">' +
                    '<span class="deepseek-ts-spin"></span><span>' + MSG_CHECK + '</span>' +
                '</span><div class="deepseek-ts-widget"></div>';

            var footer = container.querySelector('.aipkit_chat_footer');
            if (footer) {
                footer.parentNode.insertBefore(wrap, footer.nextSibling);
            } else {
                inputArea.parentNode.insertBefore(wrap, inputArea);
            }

            if (alreadyOk) {
                container.dataset.deepseekTsOk = '1';
                wrap.dataset.gone = '1';
                wrap.classList.add('deepseek-ts-vh'); // unsichtbar, bleibt im DOM für Renew
            } else {
                // Senden + Enter sperren, bis verifiziert
                lockSend(inputArea, true);
            }
            var ta = inputArea.querySelector('.aipkit_chat_input_field');
            if (ta) {
                ta.addEventListener('keydown', function (e) {
                    if (e.key !== 'Enter' || e.shiftKey) return;
                    if (container.dataset.deepseekTsOk === '1') {
                        dismiss(container); // Sicherheitsnetz: spätestens mit erster Nachricht weg
                    } else {
                        e.preventDefault();
                        e.stopPropagation();
                    }
                }, true);
            }
            var sendBtn = inputArea.querySelector('.aipkit_send_btn');
            if (sendBtn) {
                sendBtn.addEventListener('click', function () {
                    if (container.dataset.deepseekTsOk === '1') dismiss(container);
                });
            }

            // Turnstile rendern (Managed). Bei Erfolg Token serverseitig einlösen -> Cookie.
            wrap._wid = window.turnstile.render(wrap.querySelector('.deepseek-ts-widget'), {
                sitekey: SITEKEY,
                action: 'chat',
                theme: 'auto',
                callback: function (token) {
                    verifyOnServer(token, container, wrap, inputArea);
                },
                'expired-callback': function () {
                    if (container.dataset.deepseekTsOk === '1') return;
                    lockSend(inputArea, true);
                    setStatus(wrap, MSG_CHECK, '');
                },
                'error-callback': function () {
                    if (container.dataset.deepseekTsOk === '1') return;
                    lockSend(inputArea, true);
                    setStatus(wrap, MSG_ERR, 'err');
                    return true; // Turnstile-eigenes Retry erlauben
                }
            });

            instances.push({ container: container, wrap: wrap, inputArea: inputArea });
            setupSelfHealing();
        }

        // Prüfung neu auslösen, OHNE Reload: Widget wieder einblenden, Button sperren,
        // Turnstile zurücksetzen -> neuer Token -> verifyOnServer -> frischer Cookie.
        function rearm(inst) {
            var wrap = inst.wrap;
            if (!wrap || wrap.dataset.rearming === '1') return;
            wrap.dataset.rearming = '1';
            inst.container.dataset.deepseekTsOk = '';
            wrap.dataset.gone = '';
            wrap.classList.remove('deepseek-ts-vh');
            wrap.style.opacity = '1';
            lockSend(inst.inputArea, true);
            setStatus(wrap, MSG_CHECK, '');
            if (window.turnstile && wrap._wid !== undefined) {
                try { window.turnstile.reset(wrap._wid); } catch (e) {}
            }
            // Flag zurücksetzen, sobald die Prüfung wieder durch ist (oder nach Timeout).
            setTimeout(function () { wrap.dataset.rearming = ''; }, 4000);
        }

        function rearmAll() {
            for (var i = 0; i < instances.length; i++) { rearm(instances[i]); }
        }

        // Cookie aktiv frisch halten: bei Tab-Rückkehr und periodisch erneuern,
        // damit er gar nicht erst abläuft.
        function renewAll() {
            for (var i = 0; i < instances.length; i++) {
                var inst = instances[i];
                if (inst.container.dataset.deepseekTsOk !== '1') continue;
                if (window.turnstile && inst.wrap._wid !== undefined) {
                    try { window.turnstile.reset(inst.wrap._wid); } catch (e) {}
                }
            }
        }

        // Erkennt fehlgeschlagene Sende-Aufrufe wegen totem Cookie und löst die
        // Prüfung selbst neu aus — kein „Seite neu laden" nötig.
        function setupSelfHealing() {
            if (hooked) return;
            hooked = true;

            // Tab wieder aktiv / Fenster fokussiert -> Cookie vorsorglich erneuern
            document.addEventListener('visibilitychange', function () {
                if (document.visibilityState === 'visible') renewAll();
            });
            window.addEventListener('focus', renewAll);
            // Für dauerhaft offene Tabs: einmal kurz vor Ablauf der 4h erneuern.
            setInterval(renewAll, 210 * 60 * 1000); // 3,5 h

            var chatRe = /\/aipkit\/v1\/chat\//;
            var sseRe  = /aipkit_frontend_chat_stream/;

            // fetch
            if (window.fetch) {
                var _fetch = window.fetch;
                window.fetch = function (input) {
                    var url = (typeof input === 'string') ? input : (input && input.url) || '';
                    return _fetch.apply(this, arguments).then(function (resp) {
                        try {
                            // 403 von Chat (code) ODER Bildgenerator (data.ts_required) -> Pruefung neu auslösen.
                            if (resp && resp.status === 403) {
                                resp.clone().json().then(function (j) {
                                    if (j && (j.code === 'deepseek_ts_required' || j.ts_required || (j.data && j.data.ts_required))) {
                                        rearmAll();
                                    }
                                }).catch(function () {});
                            }
                        } catch (e) {}
                        return resp;
                    });
                };
            }

            // XMLHttpRequest
            var _open = XMLHttpRequest.prototype.open;
            XMLHttpRequest.prototype.open = function (m, url) {
                this._dsUrl = url;
                return _open.apply(this, arguments);
            };
            var _send = XMLHttpRequest.prototype.send;
            XMLHttpRequest.prototype.send = function () {
                var xhr = this;
                xhr.addEventListener('load', function () {
                    try {
                        if (xhr.status === 403 && chatRe.test(xhr._dsUrl || '') &&
                            (xhr.responseText || '').indexOf('deepseek_ts_required') !== -1) {
                            rearmAll();
                        }
                    } catch (e) {}
                });
                return _send.apply(this, arguments);
            };

            // EventSource (SSE-Stream)
            if (window.EventSource) {
                var _ES = window.EventSource;
                var Wrapped = function (url, cfg) {
                    var es = new _ES(url, cfg);
                    try {
                        if (sseRe.test(url)) {
                            es.addEventListener('error', function (ev) {
                                try {
                                    if (ev && ev.data && ev.data.indexOf('ts_required') !== -1) rearmAll();
                                } catch (e) {}
                            });
                        }
                    } catch (e) {}
                    return es;
                };
                Wrapped.prototype = _ES.prototype;
                Wrapped.CONNECTING = _ES.CONNECTING;
                Wrapped.OPEN = _ES.OPEN;
                Wrapped.CLOSED = _ES.CLOSED;
                window.EventSource = Wrapped;
            }
        }

        // Token beim Server einlösen -> setzt/erneuert den signierten Cookie, der die
        // Sendewege (REST + SSE) freischaltet. Läuft sowohl bei der ersten Prüfung als
        // auch bei jedem automatischen Turnstile-Refresh (dann lautlos im Hintergrund).
        function verifyOnServer(token, container, wrap, inputArea) {
            var renew = container.dataset.deepseekTsOk === '1';
            if (!renew) setStatus(wrap, MSG_CHECK, '');
            fetch(AJAXURL, {
                method: 'POST',
                credentials: 'same-origin',
                headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
                body: 'action=deepseek_ts_verify&token=' + encodeURIComponent(token)
            }).then(function (r) {
                return r.json().catch(function () { return { ok: false }; });
            }).then(function (j) {
                if (j && j.ok) {
                    container.dataset.deepseekTsOk = '1';
                    lockSend(inputArea, false);
                    if (!renew) {
                        // Erste Prüfung: kurz „✓ Bestätigt", dann ausblenden (bleibt aktiv).
                        setStatus(wrap, MSG_OK, 'ok');
                        setTimeout(function () { dismiss(container); }, 1200);
                    }
                    // renew: Cookie erneuert, nichts an der UI ändern.
                } else if (!renew) {
                    lockSend(inputArea, true);
                    setStatus(wrap, MSG_ERR, 'err');
                    if (window.turnstile && wrap._wid !== undefined) {
                        try { window.turnstile.reset(wrap._wid); } catch (e) {}
                    }
                }
                // renew-Fehlschlag: ignorieren — Turnstile refresht selbst erneut; läuft
                // der Cookie doch ab, greift serverseitig sauber der Block.
            }).catch(function () {
                if (!renew) {
                    lockSend(inputArea, true);
                    setStatus(wrap, MSG_ERR, 'err');
                }
            });
        }

        // Bildgenerator (/image/): Widget rendern, damit der Gast den dsts-Cookie bekommt.
        // Kein Button-Lock (der Server-Guard auf aipkit_generate_image ist die echte
        // Durchsetzung; der Managed-Check setzt den Cookie i.d.R. in ~1s beim Laden).
        function processImage(container) {
            if (!ready || container.dataset.deepseekTs) return;
            container.dataset.deepseekTs = '1';
            var wrap = document.createElement('div');
            wrap.className = 'deepseek-ts-wrap deepseek-ts-image';
            wrap.innerHTML =
                '<span class="deepseek-ts-status">' +
                    '<span class="deepseek-ts-spin"></span><span>' + MSG_CHECK + '</span>' +
                '</span><div class="deepseek-ts-widget"></div>';
            // UNTER den Creator-Bereich setzen (nach der Eingabe-Box), 100% breit (CSS).
            var bar = container.querySelector('.aipkit_image_generator_input_bar');
            if (bar && bar.parentNode) {
                bar.parentNode.insertBefore(wrap, bar.nextSibling);
            } else {
                container.appendChild(wrap);
            }
            // Bereits verifiziert? Widget unsichtbar starten (nur stille Erneuerung).
            if (dsTsMarker()) {
                container.dataset.deepseekTsOk = '1';
                wrap.dataset.gone = '1';
                wrap.classList.add('deepseek-ts-vh');
            }

            wrap._wid = window.turnstile.render(wrap.querySelector('.deepseek-ts-widget'), {
                sitekey: SITEKEY,
                action: 'image',
                theme: 'auto',
                callback: function (token) {
                    // inputArea = container -> lockSend findet keinen .aipkit_send_btn -> no-op.
                    verifyOnServer(token, container, wrap, container);
                },
                'expired-callback': function () {
                    if (container.dataset.deepseekTsOk === '1') return;
                    setStatus(wrap, MSG_CHECK, '');
                },
                'error-callback': function () {
                    if (container.dataset.deepseekTsOk === '1') return;
                    setStatus(wrap, MSG_ERR, 'err');
                    return true;
                }
            });

            instances.push({ container: container, wrap: wrap, inputArea: container });
            setupSelfHealing();
        }

        function scan() {
            var containers = document.querySelectorAll('.aipkit_chat_container');
            for (var i = 0; i < containers.length; i++) { process(containers[i]); }
            var ig = document.getElementById('aipkit_public_image_generator');
            if (ig) { processImage(ig); }
        }

        // Von Turnstile aufgerufen, sobald die API geladen ist
        window.deepseekTsInit = function () {
            ready = true;
            scan();
        };

        // Chat kann verzögert/dynamisch (Popup, Embed) erscheinen → beobachten + kurzes Polling
        document.addEventListener('DOMContentLoaded', scan);
        if (window.MutationObserver) {
            new MutationObserver(scan).observe(document.documentElement, {
                childList: true, subtree: true
            });
        }
        var tries = 0, iv = setInterval(function () {
            scan();
            if (++tries > 40) clearInterval(iv); // ~12s Sicherheitsnetz
        }, 300);
    })();
    </script>