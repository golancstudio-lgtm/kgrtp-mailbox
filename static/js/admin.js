const page = document.body.dataset.page;

if (page === "login") {
  startGoogle();
}

if (page === "admin") {
  startAdmin();
}

function startGoogle() {
  const status = document.getElementById("login-status");
  const clientId = document.body.dataset.clientId;
  if (!clientId) {
    status.textContent = "Google sign-in is not configured.";
    return;
  }
  if (!window.google || !google.accounts || !google.accounts.id) {
    window.setTimeout(startGoogle, 150);
    return;
  }
  google.accounts.id.initialize({
    client_id: clientId,
    callback: onCredential,
    auto_select: false,
    cancel_on_tap_outside: true,
  });
  google.accounts.id.renderButton(document.getElementById("google-button"), {
    theme: "filled_black",
    size: "large",
    text: "signin_with",
    shape: "rectangular",
    width: 320,
  });

  async function onCredential(response) {
    status.textContent = "Checking your Google account…";
    try {
      const result = await fetch("/admin/auth/google", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ credential: response.credential }),
      });
      if (result.status === 403) {
        window.location.assign("/admin/login?error=access_denied");
        return;
      }
      if (!result.ok) {
        window.location.assign("/admin/login?error=google");
        return;
      }
      window.location.assign("/admin");
    } catch (_error) {
      status.textContent = "Sign-in could not reach the mailbox service.";
    }
  }
}

function startAdmin() {
  const csrf = document.body.dataset.csrf || "";
  const account = document.body.dataset.account || "";
  document.getElementById("account-label").textContent = account;
  const form = document.getElementById("composer");
  const attachments = [];
  const subject = document.getElementById("subject");
  const body = document.getElementById("body");
  const errorBox = document.getElementById("form-error");
  const resultBox = document.getElementById("result");
  let pollTimer = 0;

  document.getElementById("logout").addEventListener("click", logout);
  document.getElementById("add-attachment").addEventListener("click", () => addAttachment());
  document.getElementById("validate-player").addEventListener("click", validatePlayer);
  form.addEventListener("input", refreshPreview);
  form.addEventListener("change", onModeChange);
  form.addEventListener("submit", submitMessage);
  subject.addEventListener("input", () => count(subject, "subject-count", 120));
  body.addEventListener("input", () => count(body, "body-count", 5000));
  onModeChange();
  refreshPreview();

  function onModeChange() {
    const broadcast = selectedMode() === "broadcast";
    document.getElementById("player-fields").hidden = broadcast;
    document.getElementById("broadcast-warning").hidden = !broadcast;
    document.getElementById("submit").textContent = broadcast ? "Send to All Players" : "Send Message";
    refreshPreview();
  }

  function selectedMode() {
    const selected = form.querySelector("input[name=mode]:checked");
    return selected ? selected.value : "player";
  }

  function addAttachment() {
    attachments.push({ type: "currency", currency: "coins", piece: "pawn", stars: 1, amount: 1 });
    renderAttachments();
    refreshPreview();
  }

  function renderAttachments() {
    const root = document.getElementById("attachments");
    root.replaceChildren();
    attachments.forEach((attachment, index) => {
      const card = document.createElement("div");
      card.className = "attachment";
      const currency = attachment.type === "currency";
      card.innerHTML = `
        <div class="attachment-head">
          <strong>Attachment ${index + 1}</strong>
          <button type="button" class="remove">Remove</button>
        </div>
        <label>Type
          <select data-field="type">
            <option value="currency" ${currency ? "selected" : ""}>Currency</option>
            <option value="piece" ${currency ? "" : "selected"}>Chess Piece</option>
          </select>
        </label>
        <div class="split">
          <label>${currency ? "Currency" : "Piece"}
            ${currency ? currencySelect(attachment) : pieceSelect(attachment)}
          </label>
          ${currency ? "" : `<label>Stars<input data-field="stars" type="number" min="1" max="25" step="1" value="${escapeAttr(attachment.stars)}"></label>`}
          <label>Amount<input data-field="amount" type="number" min="1" step="1" value="${escapeAttr(attachment.amount)}"></label>
        </div>
        <p class="hint">${escapeText(describe(attachment))}</p>`;
      card.querySelector(".remove").addEventListener("click", () => {
        attachments.splice(index, 1);
        renderAttachments();
        refreshPreview();
      });
      card.querySelectorAll("[data-field]").forEach((field) => {
        field.addEventListener("input", () => {
          readAttachment(index, field);
          refreshPreview();
        });
        field.addEventListener("change", () => {
          readAttachment(index, field);
          if (field.dataset.field === "type") {
            renderAttachments();
          }
          refreshPreview();
        });
      });
      root.appendChild(card);
    });
  }

  function readAttachment(index, field) {
    const attachment = attachments[index];
    const name = field.dataset.field;
    if (name === "type") {
      attachment.type = field.value;
    } else if (name === "currency") {
      attachment.currency = field.value;
    } else if (name === "piece") {
      attachment.piece = field.value;
    } else if (name === "stars") {
      const parsed = Number(field.value);
      if (Number.isInteger(parsed)) {
        attachment.stars = clampInteger(parsed, 1, 25);
      }
    } else if (name === "amount") {
      const parsed = Number(field.value);
      if (Number.isInteger(parsed)) {
        attachment.amount = clampInteger(parsed, 1, 4294967295);
      }
    }
  }

  function buildMessage() {
    const expiresValue = document.getElementById("expires").value;
    const payload = {
      sender: {
        type: document.getElementById("sender-type").value,
        id: null,
        displayName: document.getElementById("display-name").value,
      },
      category: document.getElementById("category").value,
      subject: subject.value,
      body: body.value,
      expiresAt: expiresValue ? toUtc(expiresValue) : null,
      attachments: attachments.map(toAttachment),
      metadata: {},
    };
    const mode = selectedMode();
    return {
      mode,
      playerId: mode === "player" ? document.getElementById("player-id").value.trim() : null,
      confirmBroadcast: mode === "broadcast" && document.getElementById("confirm-broadcast").checked,
      ...payload,
    };
  }

  function refreshPreview() {
    count(subject, "subject-count", 120);
    count(body, "body-count", 5000);
    const message = buildMessage();
    const lines = [
      `<p><strong>${escapeText(message.subject || "Untitled message")}</strong></p>`,
      `<p>${escapeText(message.body || "")}</p>`,
      `<p>From ${escapeText(message.sender.displayName || "King's Gambit")} · ${escapeText(message.category)}</p>`,
    ];
    if (message.attachments.length === 0) {
      lines.push("<p>No attachments.</p>");
    } else {
      message.attachments.forEach((attachment) => {
        lines.push(`<p>${escapeText(readableAttachment(attachment))}</p>`);
      });
    }
    document.getElementById("readable").innerHTML = lines.join("");
    document.getElementById("json-preview").textContent = JSON.stringify(message, null, 2);
  }

  async function validatePlayer() {
    const target = document.getElementById("player-result");
    target.textContent = "Checking player…";
    const response = await postJson("/admin/api/validate-player", {
      playerId: document.getElementById("player-id").value.trim(),
    });
    if (!response.ok) {
      target.textContent = response.message;
      return;
    }
    target.textContent = response.data.disabled
      ? "Player exists but is disabled. A direct send still delivers. Broadcasts skip disabled players."
      : "Player exists and is enabled.";
  }

  async function submitMessage(event) {
    event.preventDefault();
    window.clearInterval(pollTimer);
    hide(errorBox);
    hide(resultBox);
    const message = buildMessage();
    const localError = localValidation(message);
    if (localError) {
      showError(localError);
      return;
    }
    const button = document.getElementById("submit");
    button.disabled = true;
    const response = await postJson("/admin/api/messages", message);
    button.disabled = false;
    if (!response.ok) {
      showError(response.message);
      return;
    }
    if (response.data.delivery === "broadcast") {
      showResult(`Broadcast accepted. ID ${response.data.broadcastId}. Delivery is still in progress.`);
      pollBroadcast(response.data.broadcastId);
      return;
    }
    const sent = response.data.message;
    showResult(`Message ${sent.id} was stored for ${sent.recipientId}. The game grants any attached rewards when the player claims it.`);
  }

  function pollBroadcast(broadcastId) {
    const tick = async () => {
      const response = await fetch(`/admin/api/broadcasts/${encodeURIComponent(broadcastId)}`, {
        credentials: "same-origin",
      });
      const data = await response.json().catch(() => null);
      if (!response.ok) {
        showError(errorMessage(data, response.status));
        window.clearInterval(pollTimer);
        return;
      }
      const status = data.status;
      const total = data.totalPlayers === null || data.totalPlayers === undefined ? "unknown" : data.totalPlayers;
      const failureLines = (data.recentFailures || []).slice(0, 5).map((failure) => `${failure.playerId}: ${failure.code}`);
      showResult(
        `Broadcast ${broadcastId} is ${status}. Processed ${data.processed}, succeeded ${data.succeeded}, failed ${data.failed}. Total players: ${total}.` +
        (failureLines.length ? ` Recent failures: ${failureLines.join("; ")}.` : "")
      );
      if (status === "completed" || status === "completed_with_errors" || status === "failed") {
        window.clearInterval(pollTimer);
      }
    };
    tick();
    pollTimer = window.setInterval(tick, 5000);
  }

  async function logout() {
    await fetch("/admin/logout", {
      method: "POST",
      credentials: "same-origin",
      headers: { "X-CSRF-Token": csrf },
    });
    window.location.assign("/admin/login");
  }

  async function postJson(url, payload) {
    try {
      const response = await fetch(url, {
        method: "POST",
        credentials: "same-origin",
        headers: {
          "Content-Type": "application/json",
          "X-CSRF-Token": csrf,
        },
        body: JSON.stringify(payload),
      });
      const data = await response.json().catch(() => null);
      if (!response.ok) {
        return { ok: false, message: errorMessage(data, response.status) };
      }
      return { ok: true, data };
    } catch (_error) {
      return { ok: false, message: "The mailbox service could not be reached." };
    }
  }
}

function localValidation(message) {
  if (codePoints(message.subject) < 1 || codePoints(message.subject) > 120) {
    return "Subject must be 1-120 characters.";
  }
  if (codePoints(message.body) < 1 || codePoints(message.body) > 5000) {
    return "Body must be 1-5000 characters.";
  }
  if (message.mode === "player" && !message.playerId) {
    return "Enter a player ID.";
  }
  if (message.mode === "broadcast" && !message.confirmBroadcast) {
    return "Confirm the broadcast before sending to every player.";
  }
  const seen = new Set();
  for (const attachment of message.attachments) {
    const key = `${attachment.type}:${attachment.id}`;
    if (seen.has(key)) {
      return "Remove duplicate attachments.";
    }
    seen.add(key);
    if (!Number.isInteger(attachment.amount) || attachment.amount < 1) {
      return "Attachment amounts must be positive integers.";
    }
  }
  return "";
}

function toUtc(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return null;
  }
  return date.toISOString().replace(/\.\d{3}Z$/, "Z");
}

function toAttachment(attachment) {
  if (attachment.type === "piece") {
    return { type: "piece", id: `${attachment.piece}_${attachment.stars}`, amount: Number(attachment.amount) };
  }
  return { type: "currency", id: attachment.currency, amount: Number(attachment.amount) };
}

function describe(attachment) {
  return readableAttachment(toAttachment(attachment));
}

function readableAttachment(attachment) {
  if (attachment.type === "currency") {
    return `${attachment.amount} ${attachment.id}`;
  }
  const parts = String(attachment.id).split("_");
  return `${attachment.amount} × ${parts[0]} (${parts[1]} stars)`;
}

function currencySelect(attachment) {
  return `<select data-field="currency">
    <option value="coins" ${attachment.currency === "coins" ? "selected" : ""}>Coins</option>
    <option value="diamonds" ${attachment.currency === "diamonds" ? "selected" : ""}>Diamonds</option>
  </select>`;
}

function pieceSelect(attachment) {
  const pieces = ["pawn", "knight", "bishop", "rook", "queen", "king"];
  return `<select data-field="piece">${pieces.map((piece) => `<option value="${piece}" ${attachment.piece === piece ? "selected" : ""}>${title(piece)}</option>`).join("")}</select>`;
}

function count(field, targetId, max) {
  document.getElementById(targetId).textContent = `${codePoints(field.value)}/${max}`;
}

function codePoints(value) {
  return Array.from(value || "").length;
}

function clampInteger(value, min, max) {
  const parsed = Number(value);
  if (!Number.isInteger(parsed)) {
    return min;
  }
  return Math.min(max, Math.max(min, parsed));
}

function showError(message) {
  const box = document.getElementById("form-error");
  box.hidden = false;
  box.textContent = message;
}

function showResult(message) {
  const box = document.getElementById("result");
  box.hidden = false;
  box.textContent = message;
}

function hide(element) {
  element.hidden = true;
  element.textContent = "";
}

function errorMessage(data, status) {
  if (data && data.error && data.error.message) {
    return `${data.error.message} (${data.error.code || status})`;
  }
  return `Request failed (${status}).`;
}

function escapeText(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function escapeAttr(value) {
  return escapeText(value).replaceAll('"', "&quot;");
}

function title(value) {
  return value.charAt(0).toUpperCase() + value.slice(1);
}
