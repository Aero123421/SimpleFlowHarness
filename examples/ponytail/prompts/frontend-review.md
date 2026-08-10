# Frontend review contract

Review the current frontend change for behavior, accessibility, and native-first implementation.

Check keyboard and focus behavior, semantic markup, labels, reduced-motion expectations, responsive behavior,
browser-native controls/APIs, loading/error states, and whether JavaScript or a dependency duplicates HTML/CSS/browser features.

Do not demand a custom component where a native control satisfies the requirement.
Do not remove required accessibility to shrink the diff.

If no blocking issue remains, end with exactly:
REVIEW: PASS

Otherwise end with exactly:
REVIEW: REVISE
