#!/usr/bin/env python3
"""Fix encoding issues in Python scripts for Windows compatibility."""
import os
import re
import sys

def fix_file(path):
    """Add encoding='utf-8' to all open() calls that write files."""
    with open(path, 'r', encoding='utf-8') as f:
        content = f.read()

    original = content

    # Pattern 1: open(path, "w") -> open(path, "w", encoding="utf-8")
    content = re.sub(
        r'open\(([^)]+),\s*["\']w["\']\)',
        r'open(\1, "w", encoding="utf-8")',
        content
    )

    # Pattern 2: json.dump(..., open(path, "w")) -> json.dump(..., open(path, "w", encoding="utf-8"))
    content = re.sub(
        r'json\.dump\(([^,]+),\s*open\(([^)]+),\s*["\']w["\']\)',
        r'json.dump(\1, open(\2, "w", encoding="utf-8")',
        content
    )

    # Pattern 3: with open(path, "w") as f: -> with open(path, "w", encoding="utf-8") as f:
    content = re.sub(
        r'with\s+open\(([^)]+),\s*["\']w["\']\)',
        r'with open(\1, "w", encoding="utf-8")',
        content
    )

    if content != original:
        with open(path, 'w', encoding='utf-8') as f:
            f.write(content)
        return True
    return False

def main():
    scripts_dir = os.path.join(os.path.dirname(__file__),
                               'skills/evomu-adversarial-review/scripts')

    fixed = []
    for root, dirs, files in os.walk(scripts_dir):
        for fname in files:
            if fname.endswith('.py'):
                path = os.path.join(root, fname)
                if fix_file(path):
                    fixed.append(os.path.relpath(path, scripts_dir))

    if fixed:
        print(f"Fixed {len(fixed)} files:")
        for f in fixed:
            print(f"  - {f}")
    else:
        print("No files needed fixing.")

    return 0

if __name__ == '__main__':
    sys.exit(main())
