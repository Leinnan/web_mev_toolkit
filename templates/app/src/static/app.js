// Client behaviour. The CSP forbids inline scripts, so everything lives here.
(function () {
  "use strict";

  // Every htmx request carries the session's CSRF token.
  document.addEventListener("htmx:configRequest", function (event) {
    var meta = document.querySelector('meta[name="csrf-token"]');
    var token = meta ? meta.getAttribute("content") : "";
    if (token) event.detail.headers["X-CSRF-Token"] = token;
  });

  // `HxTrigger::event_with("toast", ...)` from the server shows a message. It's
  // set as text, so it can't inject markup.
  document.body.addEventListener("toast", function (event) {
    var toasts = document.getElementById("toasts");
    if (!toasts) return;
    var kind = event.detail.kind || "info";
    var toast = document.createElement("p");
    toast.className = "notice notice-" + kind + " bg-panel shadow-sm";
    toast.setAttribute("role", "status");
    toast.textContent = event.detail.message;
    toasts.appendChild(toast);
    setTimeout(function () { toast.remove(); }, 4000);
  });

  // A form that was just submitted clears, so the next note starts empty.
  document.body.addEventListener("htmx:afterRequest", function (event) {
    var form = event.detail.elt;
    if (event.detail.successful && form.matches("form[data-reset-on-success]")) form.reset();
  });
})();
