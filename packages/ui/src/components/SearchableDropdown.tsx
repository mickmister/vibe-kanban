import type { KeyboardEvent, ReactNode } from 'react';
import { useEffect, useRef } from 'react';
import { cn } from '../lib/cn';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSearchInput,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from './Dropdown';

interface SearchableDropdownProps<T> {
  /** Array of filtered items to display */
  filteredItems: T[];
  /** Currently selected value (matched against getItemKey) */
  selectedValue?: string | null;

  /** Extract unique key from item */
  getItemKey: (item: T) => string;
  /** Extract display label from item */
  getItemLabel: (item: T) => string;

  /** Called when an item is selected */
  onSelect: (item: T) => void;

  /** Trigger element (uses asChild pattern) */
  trigger: ReactNode;

  /** Search state */
  searchTerm: string;
  onSearchTermChange: (value: string) => void;

  /** Highlight state */
  highlightedIndex: number | null;
  onHighlightedIndexChange: (index: number | null) => void;

  /** Open state */
  open: boolean;
  onOpenChange: (open: boolean) => void;

  /** Keyboard handler */
  onKeyDown: (e: KeyboardEvent) => void;

  /** Class name for dropdown content */
  contentClassName?: string;
  /** Placeholder text for search input */
  placeholder?: string;
  /** Message shown when no items match */
  emptyMessage?: string;

  /** Optional badge text for each item */
  getItemBadge?: (item: T) => string | undefined;

  /** Optional icon/avatar to render before each item's label */
  getItemIcon?: (item: T) => ReactNode;
}

export function SearchableDropdown<T>({
  filteredItems,
  selectedValue,
  getItemKey,
  getItemLabel,
  onSelect,
  trigger,
  searchTerm,
  onSearchTermChange,
  highlightedIndex,
  onHighlightedIndexChange,
  open,
  onOpenChange,
  onKeyDown,
  contentClassName,
  placeholder = 'Search',
  emptyMessage = 'No items found',
  getItemBadge,
  getItemIcon,
}: SearchableDropdownProps<T>) {
  const itemRefs = useRef<Array<HTMLElement | null>>([]);

  useEffect(() => {
    if (highlightedIndex == null) return;
    itemRefs.current[highlightedIndex]?.scrollIntoView({
      block: 'nearest',
      inline: 'nearest',
    });
  }, [highlightedIndex]);

  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
      <DropdownMenuContent className={contentClassName}>
        <DropdownMenuSearchInput
          placeholder={placeholder}
          value={searchTerm}
          onValueChange={onSearchTermChange}
          onKeyDown={onKeyDown}
        />
        <DropdownMenuSeparator />
        {filteredItems.length === 0 ? (
          <div className="px-base py-half text-sm text-low text-center">
            {emptyMessage}
          </div>
        ) : (
          <div className="max-h-64 overflow-y-auto">
            {filteredItems.map((item, idx) => {
              const key = getItemKey(item);
              const isHighlighted = idx === highlightedIndex;
              const isSelected = selectedValue === key;
              return (
                <DropdownMenuItem
                  key={key || String(idx)}
                  ref={(element) => {
                    itemRefs.current[idx] = element;
                  }}
                  onSelect={() => onSelect(item)}
                  onMouseEnter={() => onHighlightedIndexChange(idx)}
                  preventFocusOnHover
                  badge={getItemBadge?.(item)}
                  className={cn(
                    isSelected && 'bg-secondary',
                    isHighlighted && 'bg-secondary'
                  )}
                >
                  {getItemIcon?.(item)}
                  {getItemLabel(item)}
                </DropdownMenuItem>
              );
            })}
          </div>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
