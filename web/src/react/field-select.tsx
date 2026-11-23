import { ListBox, Select } from "@heroui/react"
import { Check } from "lucide-react"

type Option<T extends string> = { value: T; label: string; disabled?: boolean }
const optionKey = (value: string) => `value:${value}`

export function FieldSelect<T extends string>({ label, value, options, disabled, required, onChange }: {
  label: string
  value: T
  options: readonly Option<T>[]
  disabled?: boolean
  required?: boolean
  onChange: (value: T) => void
}) {
  const selected = options.find((option) => option.value === value)
  const placeholder = required && selected?.disabled && value === "" ? selected.label : undefined
  return <Select className="rw-select" aria-label={label}
    value={selected && placeholder === undefined ? optionKey(value) : null}
    placeholder={placeholder}
    isDisabled={disabled || options.length === 0} isRequired={required}
    onChange={(key) => {
      const option = options.find(({ value }) => optionKey(value) === key)
      if (option) onChange(option.value)
    }}>
    <Select.Trigger className="rw-select-trigger">
      <Select.Value className="rw-select-value">{selected?.label}</Select.Value>
      <Select.Indicator />
    </Select.Trigger>
    <Select.Popover className="rw-select-menu">
      <ListBox className="rw-select-list">
        {options.map((option) => <ListBox.Item id={optionKey(option.value)} key={option.value}
          className="rw-select-option" textValue={option.label} isDisabled={option.disabled}>
          <span>{option.label}</span><Check size={14} aria-hidden="true" />
        </ListBox.Item>)}
      </ListBox>
    </Select.Popover>
  </Select>
}
