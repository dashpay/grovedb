# الملحق أ: مرجع أنواع العناصر الكامل

| المُميِّز | المتغير | TreeType | الحقول | حجم التكلفة | الغرض |
|---|---|---|---|---|---|
| 0 | `Item` | غير متاح | `(value, flags)` | متغير | تخزين مفتاح-قيمة أساسي |
| 1 | `Reference` | غير متاح | `(path, max_hop, flags)` | متغير | رابط بين العناصر |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | حاوية للأشجار الفرعية |
| 3 | `SumItem` | غير متاح | `(value, flags)` | متغير | يُساهم في مجموع الأب |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | يحافظ على مجموع الأحفاد |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | شجرة مجموع 128 بت |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | شجرة عدّ العناصر |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | عدّ + مجموع مدمج |
| 8 | `ItemWithSumItem` | غير متاح | `(value, sum, flags)` | متغير | عنصر مع مساهمة مجموع |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | شجرة عدّ قابلة للإثبات |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | عدّ + مجموع قابل للإثبات |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | شجرة Sinsemilla + BulkAppendTree صديقة للـ ZK |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | سجل MMR إلحاق فقط |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | سجل إلحاق فقط عالي الإنتاجية |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | تخزين ميركل كثيف ثابت السعة |

**ملاحظات:**
- المُميِّزات 11-14 هي **أشجار غير-Merk**: البيانات تعيش خارج شجرة Merk الفرعية الابن
  - جميعها الأربعة تُخزّن بيانات غير-Merk في عمود **البيانات**
  - `CommitmentTree` تُخزّن واجهة Sinsemilla إلى جانب مدخلات BulkAppendTree في نفس عمود البيانات (المفتاح `b"__ct_data__"`)
- أشجار غير-Merk لا تملك حقل `root_key` — تجزئة الجذر الخاصة بنوعها تتدفق كتجزئة Merk الابن عبر `insert_subtree`
- `CommitmentTree` تستخدم تجزئة Sinsemilla (منحنى Pallas)؛ جميع الأنواع الأخرى تستخدم Blake3
- سلوك التكلفة لأشجار غير-Merk يتبع `NormalTree` (BasicMerkNode، بدون تجميع)
- عدّاد `DenseAppendOnlyFixedSizeTree` هو `u16` (حد أقصى 65,535)؛ الارتفاعات مقيّدة بـ 1..=16

---
