/**
 * A modal confirmation (a native <dialog>): a title, a sentence, Cancel and the action. Esc,
 * Cancel or a click outside the box say no. The focus starts on Cancel, so Enter never deletes
 * by accident, and goes back where it was afterwards.
 */
export interface ConfirmOptions {
  title: string;
  body: string;
  /** The action button's text ("Delete"). */
  action: string;
  /** The action destroys something: a red button. */
  danger?: boolean;
}

export function confirmDialog(o: ConfirmOptions): Promise<boolean> {
  const before = document.activeElement as HTMLElement | null;
  const dialog = document.createElement("dialog");
  dialog.className = "confirm";
  dialog.setAttribute("aria-labelledby", "confirm-title");
  dialog.setAttribute("aria-describedby", "confirm-body");
  const h = document.createElement("h2");
  h.id = "confirm-title";
  h.textContent = o.title;
  const p = document.createElement("p");
  p.id = "confirm-body";
  p.textContent = o.body;
  const actions = document.createElement("div");
  actions.className = "confirm-actions";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "btn btn-outline";
  cancel.textContent = "Cancel";
  const ok = document.createElement("button");
  ok.type = "button";
  ok.className = o.danger ? "btn btn-destructive" : "btn btn-default";
  ok.textContent = o.action;
  actions.append(cancel, ok);
  dialog.append(h, p, actions);
  document.body.append(dialog);

  return new Promise((resolve) => {
    let answer = false;
    const close = (yes: boolean) => {
      answer = yes;
      dialog.close();
    };
    cancel.addEventListener("click", () => close(false));
    ok.addEventListener("click", () => close(true));
    // A click on the backdrop (the dialog element itself, outside its content box).
    dialog.addEventListener("click", (e) => {
      if (e.target !== dialog) return;
      const r = dialog.getBoundingClientRect();
      if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) close(false);
    });
    dialog.addEventListener("close", () => {
      dialog.remove();
      before?.focus?.();
      resolve(answer);
    });
    dialog.showModal();
    cancel.focus();
  });
}

export interface PromptOptions {
  title: string;
  body: string;
  /** The field's label, and an example in it. */
  label: string;
  placeholder?: string;
  /** The longest answer (characters). */
  max?: number;
  action: string;
}

/**
 * A modal with one text field: the text (maybe "") when the action is chosen, null for Cancel,
 * Esc or a click outside. Enter in the field chooses the action.
 */
export function promptDialog(o: PromptOptions): Promise<string | null> {
  const before = document.activeElement as HTMLElement | null;
  const dialog = document.createElement("dialog");
  dialog.className = "confirm";
  dialog.setAttribute("aria-labelledby", "prompt-title");
  dialog.setAttribute("aria-describedby", "prompt-body");
  const form = document.createElement("form");
  form.method = "dialog";
  const h = document.createElement("h2");
  h.id = "prompt-title";
  h.textContent = o.title;
  const p = document.createElement("p");
  p.id = "prompt-body";
  p.textContent = o.body;
  const label = document.createElement("label");
  label.className = "prompt-field";
  const name = document.createElement("span");
  name.textContent = o.label;
  const input = document.createElement("input");
  input.className = "input";
  input.type = "text";
  input.placeholder = o.placeholder ?? "";
  if (o.max) input.maxLength = o.max;
  label.append(name, input);
  const actions = document.createElement("div");
  actions.className = "confirm-actions";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "btn btn-outline";
  cancel.textContent = "Cancel";
  const ok = document.createElement("button");
  ok.type = "submit";
  ok.className = "btn btn-default";
  ok.textContent = o.action;
  actions.append(cancel, ok);
  form.append(h, p, label, actions);
  dialog.append(form);
  document.body.append(dialog);

  return new Promise((resolve) => {
    let answer: string | null = null;
    cancel.addEventListener("click", () => dialog.close());
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      answer = input.value.trim();
      dialog.close();
    });
    dialog.addEventListener("click", (e) => {
      if (e.target !== dialog) return;
      const r = dialog.getBoundingClientRect();
      if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) dialog.close();
    });
    dialog.addEventListener("close", () => {
      dialog.remove();
      before?.focus?.();
      resolve(answer);
    });
    dialog.showModal();
    input.focus();
  });
}
