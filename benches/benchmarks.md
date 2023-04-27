Time to walk linux's source tree on iMac (Retina 5K, 27-inch, Late 2015):

|                           | threads  | jwalk-meta | ignore     | walkdir      |
|---------------------------|----------|------------|------------|--------------|
| rayon, unsorted           | 8        | 14.111 ms  |  -         | -            |
| rayon, unsorted, metadata | 8        | 31.926 ms  |  -         | -            |
| unsorted                  | 8        | 19.625 ms  | 27.354 ms  | -            |
| sorted                    | 8        | 21.319 ms  | 40.736 ms  | -            |
| sorted, metadata          | 8        | 45.295 ms  | 55.833 ms  | -            |
| sorted, first 100         | 8        | 0.9844 ms  | -          | -            |
| unsorted                  | 2        | 54.793 ms  | 56.602 ms  | -            |
| unsorted                  | 1        | 76.651 ms  | -          | 62.261 ms    |
| sorted                    | 1        | 84.278 ms  | -          | 99.857 ms    |
| sorted, metadata          | 1        | 203.40 ms  | -          | 204.51 ms    |

## Notes

Comparing the performance of `jwalk-meta`, `ignore`, and `walkdir` and how well they
can use multiple threads.

Options:

- "unsorted" means entries are returned in `read_dir` order.
- "sorted" means entries are returned sorted by name.
- "metadata" means filesystem metadata is loaded for each entry.
- "first 100" means only first 100 entries are taken.