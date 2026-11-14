import { defineComponent, h, ref } from "vue"

function extractOption(item, valueKey = "value", labelKey = "label") {
  if (typeof item === "string" || typeof item === "number") {
    return { value: item, label: String(item), disabled: false }
  }
  if (item && typeof item === "object") {
    return {
      value: valueKey ? item[valueKey] : item,
      label: String(item[labelKey] ?? item.label ?? item[valueKey] ?? ""),
      disabled: Boolean(item.disabled),
    }
  }
  return { value: item, label: String(item ?? ""), disabled: false }
}

const UApp = defineComponent({
  name: "UApp",
  setup(_, { slots }) {
    return () => slots.default?.() ?? null
  },
})

const UIcon = defineComponent({
  name: "UIcon",
  props: { name: { type: String, default: "" } },
  setup(props, { attrs }) {
    return () => h("span", { ...attrs, "data-ui-icon": props.name })
  },
})

const UButton = defineComponent({
  name: "UButton",
  props: {
    label: { type: String, default: undefined },
    color: { type: String, default: undefined },
    variant: { type: String, default: undefined },
    size: { type: String, default: undefined },
    icon: { type: String, default: undefined },
    leadingIcon: { type: String, default: undefined },
    trailingIcon: { type: String, default: undefined },
    loading: { type: Boolean, default: false },
    disabled: { type: Boolean, default: false },
    square: { type: Boolean, default: false },
    block: { type: Boolean, default: false },
    to: { type: [String, Object], default: undefined },
    href: { type: String, default: undefined },
    type: { type: String, default: "button" },
  },
  setup(props, { slots, attrs }) {
    return () => {
      const children = []
      if (slots.leading) children.push(...slots.leading({ ui: {} }))
      if (slots.default) children.push(...slots.default({ ui: {} }))
      else if (props.label !== undefined) children.push(props.label)
      if (slots.trailing) children.push(...slots.trailing({ ui: {} }))
      if (props.to !== undefined || props.href !== undefined) {
        return h("a", {
          ...attrs,
          href: props.href,
          to: props.to,
          "aria-disabled": props.disabled || props.loading ? "true" : undefined,
        }, children)
      }
      return h("button", {
        ...attrs,
        type: props.type,
        disabled: Boolean(props.disabled || props.loading),
      }, children)
    }
  },
})

const UInput = defineComponent({
  name: "UInput",
  inheritAttrs: false,
  props: {
    modelValue: { type: [String, Number, null], default: "" },
    type: { type: String, default: "text" },
    placeholder: { type: String, default: undefined },
    disabled: { type: Boolean, default: false },
    required: { type: Boolean, default: false },
    readonly: { type: Boolean, default: false },
    id: { type: String, default: undefined },
    name: { type: String, default: undefined },
    autocomplete: { type: String, default: undefined },
    color: { type: String, default: undefined },
    variant: { type: String, default: undefined },
    size: { type: String, default: undefined },
    icon: { type: String, default: undefined },
    leadingIcon: { type: String, default: undefined },
    trailingIcon: { type: String, default: undefined },
    highlight: { type: Boolean, default: false },
    modelModifiers: { type: Object, default: undefined },
    ui: { type: Object, default: undefined },
  },
  emits: ["update:modelValue", "input", "change", "blur"],
  setup(props, { emit, slots, attrs, expose }) {
    const inputRef = ref(null)
    expose({ inputRef })
    return () => {
      const inputEl = h("input", {
        ...attrs,
        ref: inputRef,
        id: props.id,
        name: props.name,
        type: props.type,
        value: props.modelValue ?? "",
        placeholder: props.placeholder,
        disabled: props.disabled,
        required: props.required,
        readonly: props.readonly,
        autocomplete: props.autocomplete,
        "onUpdate:modelValue": (value) => emit("update:modelValue", value),
        onInput: (event) => {
          const val = event?.target?.value ?? event
          emit("input", event)
          emit("update:modelValue", props.modelModifiers?.number ? Number(val) : val)
        },
        onChange: (event) => emit("change", event),
        onBlur: (event) => emit("blur", event),
      })
      if (!slots.leading && !slots.trailing) return inputEl
      return [slots.leading?.({ ui: {} }), inputEl, slots.trailing?.({ ui: {} })]
    }
  },
})

const UTextarea = defineComponent({
  name: "UTextarea",
  inheritAttrs: false,
  props: {
    modelValue: { type: [String, Number, null], default: "" },
    placeholder: { type: String, default: undefined },
    disabled: { type: Boolean, default: false },
    required: { type: Boolean, default: false },
    rows: { type: Number, default: 3 },
    autoresize: { type: Boolean, default: false },
    maxrows: { type: Number, default: undefined },
    id: { type: String, default: undefined },
    name: { type: String, default: undefined },
    color: { type: String, default: undefined },
    variant: { type: String, default: undefined },
    size: { type: String, default: undefined },
    highlight: { type: Boolean, default: false },
    ui: { type: Object, default: undefined },
  },
  emits: ["update:modelValue", "input", "change", "blur"],
  setup(props, { emit, attrs, expose }) {
    const textareaRef = ref(null)
    expose({ textareaRef, autoResize() {} })
    return () => h("textarea", {
      ...attrs,
      ref: textareaRef,
      id: props.id,
      name: props.name,
      value: props.modelValue ?? "",
      placeholder: props.placeholder,
      disabled: props.disabled,
      required: props.required,
      rows: props.rows,
      "onUpdate:modelValue": (value) => emit("update:modelValue", value),
      onInput: (event) => {
        emit("update:modelValue", event?.target?.value ?? event)
        emit("input", event)
      },
      onChange: (event) => emit("change", event),
      onBlur: (event) => emit("blur", event),
    })
  },
})

const USelect = defineComponent({
  name: "USelect",
  props: {
    modelValue: { type: [String, Number, Object, Array, Boolean, null], default: undefined },
    items: { type: Array, default: () => [] },
    valueKey: { type: String, default: "value" },
    labelKey: { type: String, default: "label" },
    placeholder: { type: String, default: undefined },
    disabled: { type: Boolean, default: false },
    required: { type: Boolean, default: false },
    id: { type: String, default: undefined },
    name: { type: String, default: undefined },
    size: { type: String, default: undefined },
    color: { type: String, default: undefined },
    variant: { type: String, default: undefined },
    multiple: { type: Boolean, default: false },
  },
  emits: ["update:modelValue", "change"],
  setup(props, { emit, attrs }) {
    return () => {
      const flat = (props.items ?? []).flatMap((group) => Array.isArray(group) ? group : [group])
      const options = flat
        .filter((item) => !(item && typeof item === "object" && (item.type === "separator" || item.type === "label")))
        .map((item) => {
          const opt = extractOption(item, props.valueKey, props.labelKey)
          return h("option", { value: opt.value, disabled: opt.disabled }, opt.label)
        })
      return h("select", {
        ...attrs,
        id: props.id,
        name: props.name,
        value: props.modelValue,
        disabled: props.disabled,
        required: props.required,
        "onUpdate:modelValue": (value) => emit("update:modelValue", value),
        onChange: (event) => {
          const val = event?.target?.value
          emit("update:modelValue", val)
          emit("change", event)
        },
      }, options)
    }
  },
})

const USelectMenu = defineComponent({
  name: "USelectMenu",
  props: {
    modelValue: { type: [String, Number, Object, Array, Boolean, null], default: undefined },
    items: { type: Array, default: () => [] },
    valueKey: { type: String, default: undefined },
    labelKey: { type: String, default: "label" },
    placeholder: { type: String, default: undefined },
    searchInput: { type: [Boolean, Object], default: true },
    createItem: { type: [Boolean, String, Object], default: false },
    disabled: { type: Boolean, default: false },
    size: { type: String, default: undefined },
    color: { type: String, default: undefined },
    variant: { type: String, default: undefined },
  },
  emits: ["update:modelValue", "change", "create"],
  setup(props, { emit, attrs }) {
    const open = ref(false)
    return () => {
      const flat = (props.items ?? []).flatMap((group) => Array.isArray(group) ? group : [group])
      const options = flat
        .filter((item) => !(item && typeof item === "object" && (item.type === "separator" || item.type === "label")))
        .map((item) => extractOption(item, props.valueKey, props.labelKey))
      const selected = options.find((opt) => opt.value === props.modelValue)
      return h("div", { class: "ui-select-menu-stub" }, [
        h("button", {
          ...attrs,
          type: "button",
          role: "combobox",
          "aria-expanded": open.value,
          disabled: props.disabled,
          onClick: () => { if (!props.disabled) open.value = !open.value },
        }, selected?.label ?? props.placeholder ?? ""),
        h("div", { role: "listbox" }, options.map((opt) => h("button", {
          type: "button",
          role: "option",
          "aria-selected": opt.value === props.modelValue,
          disabled: opt.disabled,
          onClick: () => {
            if (opt.disabled) return
            emit("update:modelValue", opt.value)
            emit("change", { target: { value: opt.value } })
            open.value = false
          },
        }, opt.label))),
      ])
    }
  },
})

const UCheckbox = defineComponent({
  name: "UCheckbox",
  props: {
    modelValue: { type: [Boolean, String, Array], default: false },
    value: { type: [String, Number, Boolean, Object], default: undefined },
    label: { type: String, default: undefined },
    description: { type: String, default: undefined },
    disabled: { type: Boolean, default: false },
    variant: { type: String, default: undefined },
    size: { type: String, default: undefined },
  },
  emits: ["update:modelValue", "change"],
  setup(props, { emit, slots, attrs }) {
    return () => h("label", { class: "ui-checkbox-stub" }, [
      h("input", {
        ...attrs,
        type: "checkbox",
        value: props.value,
        checked: Boolean(props.modelValue),
        disabled: props.disabled,
        "onUpdate:modelValue": (val) => emit("update:modelValue", val),
        onChange: (event) => {
          emit("update:modelValue", Boolean(event?.target?.checked ?? !props.modelValue))
          emit("change", event)
        },
      }),
      slots.label ? slots.label({ label: props.label }) : (props.label ? h("span", props.label) : null),
      slots.description ? slots.description({ description: props.description }) : (props.description ? h("span", props.description) : null),
    ])
  },
})

const USwitch = defineComponent({
  name: "USwitch",
  props: {
    modelValue: { type: Boolean, default: false },
    label: { type: String, default: undefined },
    description: { type: String, default: undefined },
    disabled: { type: Boolean, default: false },
    loading: { type: Boolean, default: false },
    size: { type: String, default: undefined },
  },
  emits: ["update:modelValue", "change"],
  setup(props, { emit, attrs }) {
    return () => h("button", {
      ...attrs,
      type: "button",
      role: "switch",
      "aria-checked": String(Boolean(props.modelValue)),
      disabled: Boolean(props.disabled || props.loading),
      onClick: () => {
        if (props.disabled || props.loading) return
        emit("update:modelValue", !props.modelValue)
      },
    }, props.label)
  },
})

const UModal = defineComponent({
  name: "UModal",
  inheritAttrs: false,
  props: {
    open: { type: Boolean, default: false },
    title: { type: String, default: undefined },
    description: { type: String, default: undefined },
    dismissible: { type: Boolean, default: true },
    close: { type: [Boolean, Object], default: true },
    ui: { type: Object, default: undefined },
  },
  emits: ["update:open", "close:prevent"],
  setup(props, { emit, slots, attrs }) {
    const close = () => emit("update:open", false)
    return () => {
      const nodes = []
      if (slots.default) nodes.push(...slots.default({ open: props.open }))
      if (props.open) {
        nodes.push(h("div", { ...attrs, role: "dialog" }, [
          slots.content
            ? slots.content({ close })
            : [
                slots.header ? slots.header({ close }) : (props.title ? h("h2", props.title) : null),
                slots.description ? slots.description() : (props.description ? h("p", props.description) : null),
                slots.body?.({ close }),
                slots.footer?.({ close }),
              ],
        ]))
      }
      return nodes
    }
  },
})

const UDrawer = defineComponent({
  name: "UDrawer",
  props: {
    open: { type: Boolean, default: false },
    title: { type: String, default: undefined },
    description: { type: String, default: undefined },
    direction: { type: String, default: "bottom" },
  },
  emits: ["update:open"],
  setup(props, { slots, attrs }) {
    return () => {
      if (!props.open) return slots.default?.() ?? null
      return h("div", { ...attrs, role: "dialog" }, [
        slots.header?.(),
        slots.body?.(),
        slots.content?.(),
        slots.footer?.(),
      ])
    }
  },
})

const UAlert = defineComponent({
  name: "UAlert",
  props: {
    title: { type: String, default: undefined },
    description: { type: String, default: undefined },
    color: { type: String, default: undefined },
    variant: { type: String, default: undefined },
    icon: { type: String, default: undefined },
    actions: { type: Array, default: () => [] },
    close: { type: [Boolean, Object], default: false },
  },
  emits: ["update:open"],
  setup(props, { slots, attrs }) {
    return () => h("div", { role: "alert", ...attrs }, [
      slots.title ? slots.title() : (props.title ? h("p", props.title) : null),
      slots.description ? slots.description() : (props.description ? h("p", props.description) : null),
      slots.actions ? slots.actions() : (props.actions ?? []).map((act) => h("button", { type: "button", disabled: act.disabled, onClick: act.onClick }, act.label)),
    ])
  },
})

const UBadge = defineComponent({
  name: "UBadge",
  props: {
    label: { type: [String, Number], default: undefined },
    color: { type: String, default: undefined },
    variant: { type: String, default: undefined },
    size: { type: String, default: undefined },
  },
  setup(props, { slots, attrs }) {
    return () => h("span", attrs, slots.default ? slots.default({ ui: {} }) : props.label)
  },
})

const UProgress = defineComponent({
  name: "UProgress",
  props: {
    modelValue: { type: Number, default: 0 },
    max: { type: Number, default: 100 },
    color: { type: String, default: undefined },
    size: { type: String, default: undefined },
  },
  setup(props, { attrs }) {
    return () => h("div", {
      ...attrs,
      role: "progressbar",
      "aria-valuenow": props.modelValue,
      "aria-valuemax": props.max,
    })
  },
})

const UTabs = defineComponent({
  name: "UTabs",
  props: {
    modelValue: { type: [String, Number], default: undefined },
    items: { type: Array, default: () => [] },
    content: { type: Boolean, default: true },
  },
  emits: ["update:modelValue"],
  setup(props, { emit, slots, attrs }) {
    return () => h("div", { ...attrs, role: "tablist" }, (props.items ?? []).map((item, index) => {
      const value = item.value ?? index
      return h("button", {
        type: "button",
        role: "tab",
        "aria-selected": String(value === props.modelValue),
        disabled: item.disabled,
        onClick: () => emit("update:modelValue", value),
      }, slots.default ? slots.default({ item, index }) : item.label)
    }))
  },
})

const UTooltip = defineComponent({
  name: "UTooltip",
  props: { text: { type: String, default: undefined } },
  setup(_, { slots }) {
    return () => slots.default?.({ open: false }) ?? null
  },
})

const UPopover = defineComponent({
  name: "UPopover",
  props: { open: { type: Boolean, default: undefined } },
  emits: ["update:open"],
  setup(props, { slots }) {
    return () => [
      slots.default?.({ open: Boolean(props.open) }),
      slots.content?.({ close() {} }),
    ]
  },
})

const UDropdownMenu = defineComponent({
  name: "UDropdownMenu",
  props: { items: { type: Array, default: () => [] }, open: { type: Boolean, default: undefined } },
  emits: ["update:open"],
  setup(props, { slots }) {
    return () => {
      const groups = Array.isArray(props.items?.[0]) ? props.items : [props.items ?? []]
      return h("div", { class: "ui-dropdown-stub" }, [
        slots.default?.({ open: Boolean(props.open) }),
        ...groups.flatMap((group) => group.filter((item) => item && item.type !== "separator" && item.type !== "label").map((item) => h("button", {
          type: "button",
          role: "menuitem",
          disabled: item.disabled,
          onClick: item.onSelect,
        }, item.label))),
      ])
    }
  },
})

const UCollapsible = defineComponent({
  name: "UCollapsible",
  props: { open: { type: Boolean, default: undefined }, defaultOpen: { type: Boolean, default: false }, unmountOnHide: { type: Boolean, default: true } },
  emits: ["update:open"],
  setup(props, { slots, emit, attrs }) {
    const internal = ref(props.defaultOpen)
    return () => {
      const isOpen = props.open ?? internal.value
      return h("div", attrs, [
        slots.default?.({ open: isOpen }),
        isOpen || !props.unmountOnHide ? slots.content?.() : null,
      ])
    }
  },
})

const UFormField = defineComponent({
  name: "UFormField",
  props: {
    label: { type: String, default: undefined },
    description: { type: String, default: undefined },
    help: { type: String, default: undefined },
    error: { type: [Boolean, String], default: false },
    hint: { type: String, default: undefined },
    name: { type: String, default: undefined },
  },
  setup(props, { slots, attrs }) {
    return () => h("label", attrs, [
      slots.label ? slots.label({ label: props.label }) : (props.label ? h("span", props.label) : null),
      slots.default?.({ error: props.error }),
      props.description ? h("span", props.description) : null,
      props.help ? h("span", props.help) : null,
      typeof props.error === "string" && props.error ? h("span", props.error) : null,
    ])
  },
})

const UKbd = defineComponent({
  name: "UKbd",
  props: { value: { type: String, default: undefined } },
  setup(props, { slots, attrs }) {
    return () => h("kbd", attrs, slots.default ? slots.default() : props.value)
  },
})

const components = {
  UApp,
  UIcon,
  UButton,
  UInput,
  UTextarea,
  USelect,
  USelectMenu,
  UCheckbox,
  USwitch,
  UModal,
  UDrawer,
  UAlert,
  UBadge,
  UProgress,
  UTabs,
  UTooltip,
  UPopover,
  UDropdownMenu,
  UCollapsible,
  UFormField,
  UKbd,
}

export const nuxtUiTestPlugin = {
  install(app, overrides = {}) {
    for (const [name, component] of Object.entries({ ...components, ...overrides })) {
      app.component(name, component)
    }
  },
}

export function stubUseToast() {
  return { toasts: ref([]), add() {}, update() {}, remove() {}, clear() {} }
}
