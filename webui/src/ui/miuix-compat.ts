/**
 * Varlet 兼容层。
 *
 * 11 个视图是按 `miuix-vue` 的组件名、props 和事件形状写的（约 200 处用法）。
 * 这里把每个 `Miuix*` 名字重新导出，对外 API 保持不变，内部换成 Varlet 实现。
 * 视图侧只需要换 import 路径，模板不用动。
 *
 * 约束：
 *   - props / 事件 / 插槽与原来一致，视图代码零改动。
 *   - 不发明视觉：字号、间距、色板都来自 Varlet 的 CSS 变量（本身就是 MD3
 *     token），再在 app.scss 里指回现有的 `--omk-*` / `--m-*` 变量。
 *   - Varlet 没有对应物的（设置行、选择器、导航栏、搜索栏）用 `var-cell`
 *     加与原组件相同的 DOM 结构包一层，现有 CSS 继续生效。
 */
import { computed, defineComponent, h, ref, type Component, type PropType } from 'vue'
import {
  Button,
  Card,
  Cell,
  Divider,
  Icon,
  Input,
  Popup,
  Progress,
  Radio,
  Snackbar,
  Slider,
  Switch,
} from '@varlet/ui'

/**
 * Varlet's components are declared with a `VarComponent` interface that Vue's
 * `h()` overloads would demand instances of (`$props`, `$slots`). Erasing the
 * concrete type to `Component` restores the generic, structurally-checked
 * overload, which is what every render function here relies on.
 */
const v = (component: unknown): Component => component as Component

/** miuix 图标是 `{name, light, normal, ...}` 形状的路径对象，直接内联渲染。 */
interface MiuixIconData {
  vw?: number
  vh?: number
  d?: string
}

function iconGlyph(icon: unknown): MiuixIconData | null {
  if (!icon || typeof icon !== 'object') return null
  const record = icon as Record<string, unknown>
  const weight = ('regular' in record ? record.regular : record.normal) as MiuixIconData | undefined
  return weight && typeof weight === 'object' ? weight : null
}

export const MiuixIcon = defineComponent({
  name: 'MiuixIcon',
  props: {
    icon: { type: [Object, Function] as PropType<unknown>, required: true },
    size: { type: [Number, String], default: 24 },
    color: { type: String, default: '' },
  },
  setup(props, { attrs }) {
    return () => {
      const glyph = iconGlyph(props.icon)
      const px = typeof props.size === 'number' ? `${props.size}px` : props.size
      const color = props.color && props.color !== 'none' ? props.color : undefined
      if (!glyph || !glyph.d) {
        return h('span', { ...attrs, class: ['m-icon', attrs.class], style: { width: px, height: px } })
      }
      return h(
        'span',
        {
          ...attrs,
          class: ['m-icon', { 'm-icon--no-tint': props.color === 'none' }, attrs.class],
          style: { width: px, height: px, color },
        },
        h(
          'svg',
          {
            viewBox: `0 0 ${glyph.vw ?? 24} ${glyph.vh ?? 24}`,
            fill: 'none',
            'aria-hidden': 'true',
          },
          h('path', {
            d: glyph.d,
            transform: `matrix(1 0 0 -1 0 ${glyph.vh ?? 24})`,
            fill: 'currentColor',
            'fill-rule': 'nonzero',
            'clip-rule': 'nonzero',
          }),
        ),
      )
    }
  },
})

export const MiuixButton = defineComponent({
  name: 'MiuixButton',
  props: {
    type: { type: String, default: 'default' },
    disabled: { type: Boolean, default: false },
    busy: { type: Boolean, default: false },
  },
  emits: ['click'],
  setup(props, { slots, emit, attrs }) {
    return () =>
      h(v(Button),
        {
          ...attrs,
          // miuix 的 `type="primary"` 对应 Varlet 的 tonal 层级。
          type: props.type === 'primary' ? 'tonal' : 'text',
          class: ['m-button', attrs.class],
          disabled: props.disabled,
          loading: props.busy,
          onClick: (event: MouseEvent) => emit('click', event),
        },
        () => slots.default?.(),
      )
  },
})

export const MiuixIconButton = defineComponent({
  name: 'MiuixIconButton',
  props: {
    disabled: { type: Boolean, default: false },
    holdDown: { type: Boolean, default: false },
  },
  emits: ['click'],
  setup(props, { slots, emit, attrs }) {
    return () =>
      h(
        'button',
        {
          ...attrs,
          type: 'button',
          class: [
            'm-icon-button',
            { 'm-icon-button--disabled': props.disabled, 'm-icon-button--hold-down': props.holdDown },
            attrs.class,
          ],
          disabled: props.disabled,
          onClick: (event: MouseEvent) => {
            if (!props.disabled) emit('click', event)
          },
        },
        slots.default?.(),
      )
  },
})

export const MiuixCard = defineComponent({
  name: 'MiuixCard',
  props: {
    // miuix 的 press-feedback 是字符串（"none" / "squish"），不是布尔。
    pressFeedback: { type: [Boolean, String], default: false },
  },
  setup(props, { slots, attrs }) {
    const feedback = props.pressFeedback !== false && props.pressFeedback !== 'none'
    return () =>
      h(v(Card),
        {
          ...attrs,
          class: ['m-card', attrs.class],
          variant: 'outlined',
          hoverable: feedback,
          ripple: feedback,
        },
        () => slots.default?.(),
      )
  },
})

export const MiuixDivider = defineComponent({
  name: 'MiuixDivider',
  setup(_props, { attrs }) {
    return () => h(v(Divider), { ...attrs, class: ['m-divider', attrs.class] })
  },
})

export const MiuixSmallTitle = defineComponent({
  name: 'MiuixSmallTitle',
  props: {
    text: { type: String, default: '' },
  },
  setup(props, { slots, attrs }) {
    return () =>
      h('div', { ...attrs, class: ['m-small-title', attrs.class] }, slots.default?.() ?? props.text)
  },
})

/** 可点击的设置行外壳，`var-cell` 的构成（标题 / 描述 / 右侧）与原来一致。 */
export const MiuixBasicComponent = defineComponent({
  name: 'MiuixBasicComponent',
  props: {
    title: { type: String, default: '' },
    clickable: { type: Boolean, default: false },
    disabled: { type: Boolean, default: false },
  },
  emits: ['click'],
  setup(props, { slots, emit, attrs }) {
    return () =>
      h(v(Cell),
        {
          ...attrs,
          class: ['m-basic-component', { 'm-basic-component--disabled': props.disabled }, attrs.class],
          title: props.title,
          ripple: props.clickable && !props.disabled,
          onClick: () => {
            if (!props.disabled) emit('click')
          },
        },
        {
          default: () => slots.default?.(),
          extra: () => slots.extra?.(),
        },
      )
  },
})

export const MiuixArrowPreference = defineComponent({
  name: 'MiuixArrowPreference',
  props: {
    title: { type: String, default: '' },
    summary: { type: String, default: '' },
    disabled: { type: Boolean, default: false },
    holdDown: { type: Boolean, default: false },
  },
  emits: ['click'],
  setup(props, { emit, attrs }) {
    return () =>
      h(v(Cell), {
        ...attrs,
        class: ['m-arrow-preference', { 'm-arrow-preference--disabled': props.disabled }, attrs.class],
        title: props.title,
        description: props.summary,
        ripple: !props.disabled,
        onClick: () => {
          if (!props.disabled) emit('click')
        },
        extra: () => h(MiuixIcon, { icon: ARROW_ICON, size: 16 }),
      })
  },
})

// 内联箭头，避免为单个字形再引一层图标依赖。
const ARROW_ICON = {
  name: 'ArrowRight',
  regular: {
    vw: 24,
    vh: 24,
    d: 'M9.29 6.71a1 1 0 0 0 0 1.41L13.17 12l-3.88 3.88a1 1 0 1 0 1.42 1.41l4.58-4.58a1 1 0 0 0 0-1.42L10.7 6.71a1 1 0 0 0-1.41 0z',
  },
}

export const MiuixSwitchPreference = defineComponent({
  name: 'MiuixSwitchPreference',
  props: {
    title: { type: String, default: '' },
    summary: { type: String, default: '' },
    enabled: { type: Boolean, default: undefined },
    modelValue: { type: Boolean, default: false },
    disabled: { type: Boolean, default: false },
  },
  emits: ['update:modelValue'],
  setup(props, { emit, attrs }) {
    return () =>
      h(v(Cell), {
        ...attrs,
        class: ['m-switch-preference', { 'm-switch-preference--disabled': props.disabled }, attrs.class],
        title: props.title,
        description: props.summary,
        extra: () =>
          h(v(Switch), {
            modelValue: props.modelValue,
            disabled: props.disabled,
            'onUpdate:modelValue': (value: boolean) => emit('update:modelValue', value),
          }),
      })
  },
})

export const MiuixCheckboxPreference = defineComponent({
  name: 'MiuixCheckboxPreference',
  props: {
    title: { type: String, default: '' },
    summary: { type: String, default: '' },
    modelValue: { type: Boolean, default: false },
    disabled: { type: Boolean, default: false },
  },
  emits: ['update:modelValue'],
  setup(props, { emit, attrs }) {
    return () =>
      h(v(Cell), {
        ...attrs,
        class: ['m-checkbox-preference', { 'm-checkbox-preference--disabled': props.disabled }, attrs.class],
        title: props.title,
        description: props.summary,
        ripple: !props.disabled,
        onClick: () => {
          if (!props.disabled) emit('update:modelValue', !props.modelValue)
        },
        extra: () =>
          h(v(Switch), {
            modelValue: props.modelValue,
            disabled: props.disabled,
            size: 18,
            'onUpdate:modelValue': (value: boolean) => emit('update:modelValue', value),
          }),
      })
  },
})

/**
 * 单选行。miuix 的 `model-value` 在这批视图里传的都是**布尔**（选中即 true），
 * 选中结果通过 `@select` 交回，所以这里按布尔处理，同时兼容 `value` 写法。
 */
export const MiuixRadioButtonPreference = defineComponent({
  name: 'MiuixRadioButtonPreference',
  props: {
    title: { type: String, default: '' },
    summary: { type: String, default: '' },
    modelValue: { type: [Boolean, String, Number] as PropType<boolean | string | number>, default: false },
    value: { type: [Boolean, String, Number] as PropType<boolean | string | number>, default: undefined },
    disabled: { type: Boolean, default: false },
    location: { type: String, default: 'right' },
  },
  emits: ['update:modelValue', 'select'],
  setup(props, { emit, attrs }) {
    const selected = computed(() =>
      typeof props.modelValue === 'boolean' ? props.modelValue : props.modelValue === props.value,
    )
    const choose = () => {
      if (props.disabled) return
      emit('update:modelValue', props.value ?? true)
      emit('select', props.value ?? true)
    }
    return () =>
      h(v(Cell), {
        ...attrs,
        class: [
          'm-radio-button-preference',
          { 'm-radio-button-preference--selected': selected.value, 'm-radio-button-preference--disabled': props.disabled },
          attrs.class,
        ],
        title: props.title,
        description: props.summary,
        ripple: !props.disabled,
        onClick: choose,
        extra: () =>
          h(v(Radio), {
            checked: selected.value,
            disabled: props.disabled,
            onClick: (event: Event) => event.stopPropagation(),
            onChange: choose,
          }),
      })
  },
})

/** 下拉项。miuix 允许纯字符串数组，这里两种形态都接受。 */
export interface MiuixDropdownItem {
  text?: string
  title?: string
  value?: string | number
  color?: string
}

type DropdownEntry = string | number | MiuixDropdownItem

function dropdownLabel(entry: DropdownEntry): string {
  if (typeof entry === 'object') return entry.text ?? entry.title ?? String(entry.value ?? '')
  return String(entry)
}

function dropdownValue(entry: DropdownEntry): string | number {
  return typeof entry === 'object' ? (entry.value ?? entry.text ?? '') : entry
}

function dropdownKey(entry: DropdownEntry, index: number): string {
  return `${index}:${dropdownValue(entry)}`
}

/** 下拉选择行：点开底部选择器，选中后回传 `update:modelValue`。 */
export const MiuixDropdownPreference = defineComponent({
  name: 'MiuixDropdownPreference',
  props: {
    title: { type: String, default: '' },
    summary: { type: String, default: '' },
    modelValue: { type: [String, Number] as PropType<string | number>, default: '' },
    items: {
      type: Array as PropType<DropdownEntry[]>,
      default: () => [],
    },
    disabled: { type: Boolean, default: false },
  },
  emits: ['update:modelValue'],
  setup(props, { emit, attrs, slots }) {
    const open = ref(false)
    const label = computed(() => {
      // `modelValue` is the index for the string-array form and the value for the
      // object form, so both are tried before falling back to the raw value.
      const byValue = props.items.find((item) => dropdownValue(item) === props.modelValue)
      const byIndex = typeof props.modelValue === 'number' ? props.items[props.modelValue] : undefined
      const hit = byValue ?? byIndex
      return hit === undefined ? String(props.modelValue) : dropdownLabel(hit)
    })
    return () => [
      h(v(Cell), {
        ...attrs,
        class: ['m-dropdown-preference', { 'm-dropdown-preference--disabled': props.disabled }, attrs.class],
        title: props.title,
        description: props.summary,
        ripple: !props.disabled,
        onClick: () => {
          if (!props.disabled) open.value = true
        },
        extra: () => h('span', { class: 'm-dropdown-preference__value' }, label.value),
      }),
      h(v(Popup),
        {
          show: open.value,
          position: 'bottom',
          round: true,
          safeArea: true,
          class: 'm-picker-sheet',
          'onUpdate:show': (value: boolean) => {
            open.value = value
          },
        },
        () =>
          h('div', { class: 'm-picker-sheet__inner' }, [
            h('div', { class: 'm-picker-sheet__title' }, props.title),
            ...props.items.map((item, index) =>
              h(v(Cell), {
                key: dropdownKey(item, index),
                title: dropdownLabel(item),
                ripple: true,
                class: [
                  'm-picker-sheet__row',
                  { 'm-picker-sheet__row--active': dropdownValue(item) === props.modelValue },
                ],
                onClick: () => {
                  // String arrays report the index; object entries report `value`.
                  emit('update:modelValue', typeof item === 'object' ? dropdownValue(item) : index)
                  open.value = false
                },
              }),
            ),
            slots.default?.(),
          ]),
      ),
    ]
  },
})

export const MiuixSpinnerPreference = MiuixDropdownPreference

export const MiuixSlider = defineComponent({
  name: 'MiuixSlider',
  props: {
    modelValue: { type: Number, default: 0 },
    min: { type: Number, default: 0 },
    max: { type: Number, default: 100 },
    step: { type: Number, default: 1 },
    disabled: { type: Boolean, default: false },
    showKeyPoints: { type: Boolean, default: false },
    keyPoints: { type: Array as PropType<number[]>, default: () => [] },
  },
  emits: ['update:modelValue', 'change-end'],
  setup(props, { emit, attrs }) {
    return () =>
      h(v(Slider), {
        ...attrs,
        class: ['m-slider', attrs.class],
        modelValue: props.modelValue,
        min: props.min,
        max: props.max,
        step: props.step,
        disabled: props.disabled,
        'onUpdate:modelValue': (value: number) => emit('update:modelValue', value),
        onEnd: (value: number) => emit('change-end', value),
      })
  },
})

export const MiuixProgressIndicator = defineComponent({
  name: 'MiuixProgressIndicator',
  props: {
    type: { type: String, default: 'circular' },
    size: { type: [Number, String], default: 40 },
    strokeWidth: { type: [Number, String], default: 4 },
    value: { type: Number, default: 0 },
  },
  setup(props, { attrs }) {
    return () => {
      const px = typeof props.size === 'number' ? `${props.size}px` : props.size
      const circular = props.type !== 'linear'
      return h(v(Progress), {
        ...attrs,
        class: ['m-progress', attrs.class],
        mode: circular ? 'circular' : 'linear',
        // 没有明确进度时就是转圈等待，Varlet 用 indeterminate 表达。
        indeterminate: circular && props.value === 0,
        value: props.value,
        lineWidth: Number(props.strokeWidth),
        size: px,
      })
    }
  },
})

export const MiuixDialog = defineComponent({
  name: 'MiuixDialog',
  props: {
    modelValue: { type: Boolean, default: false },
    title: { type: String, default: '' },
    closeOnClickModal: { type: Boolean, default: true },
  },
  emits: ['update:modelValue'],
  setup(props, { slots, emit, attrs }) {
    const close = () => emit('update:modelValue', false)
    return () =>
      h(v(Popup),
        {
          ...attrs,
          show: props.modelValue,
          position: 'center',
          round: true,
          closeOnClickOverlay: props.closeOnClickModal,
          class: ['m-dialog', attrs.class],
          'onUpdate:show': (value: boolean) => emit('update:modelValue', value),
        },
        () =>
          h('div', { class: 'm-dialog__inner' }, [
            props.title ? h('div', { class: 'm-dialog__title' }, props.title) : null,
            h('div', { class: 'm-dialog__body' }, slots.default?.({ close })),
          ]),
      )
  },
})

export const MiuixBottomSheet = defineComponent({
  name: 'MiuixBottomSheet',
  props: {
    modelValue: { type: Boolean, default: false },
    title: { type: String, default: '' },
    allowDismiss: { type: Boolean, default: true },
    closeOnClickModal: { type: Boolean, default: true },
  },
  emits: ['update:modelValue', 'close'],
  setup(props, { slots, emit, attrs }) {
    const close = () => {
      emit('update:modelValue', false)
      emit('close')
    }
    return () =>
      h(v(Popup),
        {
          ...attrs,
          show: props.modelValue,
          position: 'bottom',
          round: true,
          safeArea: true,
          closeOnClickOverlay: props.closeOnClickModal && props.allowDismiss,
          class: ['m-bottom-sheet', attrs.class],
          'onUpdate:show': (value: boolean) => {
            if (value) emit('update:modelValue', true)
            else if (props.allowDismiss) close()
          },
        },
        () =>
          h('div', { class: 'm-bottom-sheet__inner' }, [
            h('div', { class: 'm-bottom-sheet__header' }, [
              h('div', { class: 'm-bottom-sheet__start' }, slots['start-action']?.()),
              props.title ? h('div', { class: 'm-bottom-sheet__title' }, props.title) : null,
              h('div', { class: 'm-bottom-sheet__end' }, slots['end-action']?.()),
            ]),
            h('div', { class: 'm-bottom-sheet__body' }, slots.default?.({ close })),
            slots.footer ? h('div', { class: 'm-bottom-sheet__footer' }, slots.footer()) : null,
          ]),
      )
  },
})

/**
 * 搜索栏。Varlet 没有独立 Search 组件，用 `var-input` 拼出同样的
 * `v-model` + `v-model:expanded` + `@focusin` 契约。
 */
export const MiuixSearchBar = defineComponent({
  name: 'MiuixSearchBar',
  props: {
    modelValue: { type: String, default: '' },
    label: { type: String, default: '' },
    cancelText: { type: String, default: '' },
    expanded: { type: Boolean, default: false },
  },
  emits: ['update:modelValue', 'update:expanded', 'focusin'],
  setup(props, { emit, attrs }) {
    return () =>
      h(v(Input), {
        ...attrs,
        class: ['m-search-bar', attrs.class],
        modelValue: props.modelValue,
        placeholder: props.label,
        clearable: true,
        variant: 'standard',
        'onUpdate:modelValue': (value: string) => emit('update:modelValue', value),
        onClick: () => emit('update:expanded', true),
        onFocus: () => emit('update:expanded', true),
        onBlur: () => emit('update:expanded', false),
      })
  },
})

export const MiuixTopAppBar = defineComponent({
  name: 'MiuixTopAppBar',
  props: {
    title: { type: String, default: '' },
    color: { type: String, default: '' },
  },
  setup(props, { slots, attrs }) {
    return () =>
      h(
        'header',
        {
          ...attrs,
          class: ['m-top-app-bar', attrs.class],
          style: props.color && props.color !== 'transparent' ? { background: props.color } : undefined,
        },
        [
          h('div', { class: 'm-top-app-bar__navigation' }, slots.navigation?.()),
          h('div', { class: 'm-top-app-bar__title' }, props.title),
          h('div', { class: 'm-top-app-bar__actions' }, slots.actions?.() ?? slots.default?.()),
        ],
      )
  },
})

/** 底部导航栏：miuix 传 `items` + `#icon` 插槽，这里用 `var-cell` 排成一行。 */
export const MiuixNavigationBar = (defineComponent as any)({
  name: 'MiuixNavigationBar',
  props: {
    modelValue: { type: Number, default: 0 },
    items: { type: Array as PropType<{ label?: string, text?: string }[]>, default: () => [] },
  } as any,
  emits: ['update:modelValue'],
  setup(props: any, { slots, emit, attrs }: any) {
    return () =>
      h(
        'nav',
        { ...attrs, class: ['m-navigation-bar', attrs.class] },
        props.items.map((item: { label?: string, text?: string }, index: number) =>
          h(
            'button',
            {
              key: index,
              type: 'button',
              class: ['m-navigation-bar__item', { 'm-navigation-bar__item--active': index === props.modelValue }],
              'aria-current': index === props.modelValue ? 'page' : undefined,
              onClick: () => emit('update:modelValue', index),
            },
            [
              slots.icon ? slots.icon({ index }) : null,
              h('span', { class: 'm-navigation-bar__label' }, item.label ?? item.text ?? ''),
            ],
          ),
        ),
      )
  },
})

/** 独立的开关控件（不带设置行）。`miuix` 的 `MiuixSwitch` 只受控于 modelValue。 */
export const MiuixSwitch = defineComponent({
  name: 'MiuixSwitch',
  props: {
    modelValue: { type: Boolean, default: false },
    disabled: { type: Boolean, default: false },
  },
  emits: ['update:modelValue'],
  setup(props, { emit, attrs }) {
    return () =>
      h(v(Switch), {
        ...attrs,
        class: ['m-switch', attrs.class],
        modelValue: props.modelValue,
        disabled: props.disabled,
        'onUpdate:modelValue': (value: boolean) => emit('update:modelValue', value),
      })
  },
})

/** 全局 snackbar 宿主，`showSnackbar` 是命令式入口。 */
let snackbarRef: { show: (text: string, duration?: number) => void } | null = null

export const MiuixSnackbarHost = defineComponent({
  name: 'MiuixSnackbarHost',
  setup(_props, { attrs }) {
    const visible = ref(false)
    const text = ref('')
    const duration = ref(2500)
    snackbarRef = {
      show: (value: string, ms?: number) => {
        text.value = value
        duration.value = ms ?? 2500
        visible.value = true
      },
    }
    return () =>
      h(v(Snackbar), {
        ...attrs,
        class: ['m-snackbar', attrs.class],
        show: visible.value,
        content: text.value,
        position: 'bottom',
        duration: duration.value,
        onClosed: () => {
          visible.value = false
        },
        'onUpdate:show': (value: boolean) => {
          visible.value = value
        },
      })
  },
})

export interface SnackbarOptions {
  message: string
  /** miuix 用 `'long'` 表示更久的停留时间，不是毫秒数。 */
  duration?: number | 'long' | 'short'
  withDismissAction?: boolean
}

export function showSnackbar(input: string | SnackbarOptions): void {
  const message = typeof input === 'string' ? input : input.message
  const duration = typeof input === 'string' ? undefined : input.duration
  const ms = duration === 'long' ? 6000 : duration === 'short' ? 2000 : typeof duration === 'number' ? duration : 2500
  snackbarRef?.show(message, ms)
}

export const MiuixLoading = Icon
