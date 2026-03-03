# مقدمة — ما هو GroveDB؟

## الفكرة الأساسية

GroveDB هو **بنية بيانات هرمية موثّقة تشفيرياً** — وهو في جوهره *بستان*
(شجرة من الأشجار) مبني على أشجار Merkle AVL (شجرة ميركل إيه في إل). كل عقدة في قاعدة البيانات هي جزء من شجرة
موثّقة تشفيرياً، وكل شجرة يمكن أن تحتوي على أشجار أخرى كأبناء،
مما يشكّل تسلسلاً هرمياً عميقاً من الحالة القابلة للتحقق.

```mermaid
graph TD
    subgraph root["Root Merk Tree"]
        R_contracts["&quot;contracts&quot;<br/><i>Tree</i>"]
        R_identities["&quot;identities&quot;<br/><i>Tree</i>"]
        R_balances["&quot;balances&quot;<br/><i>SumTree</i>"]
        R_contracts --- R_identities
        R_contracts --- R_balances
    end

    subgraph ident["Identities Merk"]
        I_bob["&quot;bob&quot;<br/><i>Tree</i>"]
        I_alice["&quot;alice&quot;<br/><i>Tree</i>"]
        I_carol["&quot;carol&quot;<br/><i>Item</i>"]
        I_bob --- I_alice
        I_bob --- I_carol
    end

    subgraph contracts["Contracts Merk"]
        C_c2["&quot;C2&quot;<br/><i>Item</i>"]
        C_c1["&quot;C1&quot;<br/><i>Item</i>"]
        C_c3["&quot;C3&quot;<br/><i>Item</i>"]
        C_c2 --- C_c1
        C_c2 --- C_c3
    end

    subgraph balances["Balances SumTree — sum=5300"]
        B_bob["&quot;bob&quot;<br/>SumItem(2500)"]
        B_al["&quot;alice&quot;<br/>SumItem(2000)"]
        B_eve["&quot;eve&quot;<br/>SumItem(800)"]
        B_bob --- B_al
        B_bob --- B_eve
    end

    subgraph alice_merk["Alice Merk"]
        A_name["&quot;name&quot; → Alice"]
        A_bal["&quot;balance&quot; → 1000"]
    end

    subgraph bob_merk["Bob Merk"]
        Bo_name["&quot;name&quot; → Bob"]
    end

    R_identities -.->|subtree| ident
    R_contracts -.->|subtree| contracts
    R_balances -.->|subtree| balances
    I_alice -.->|subtree| alice_merk
    I_bob -.->|subtree| bob_merk

    style root fill:#e8f4fd,stroke:#2980b9,stroke-width:2px
    style ident fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style contracts fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style balances fill:#fdedec,stroke:#e74c3c,stroke-width:2px
    style alice_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
    style bob_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
```

> كل مربع ملون هو **شجرة Merk منفصلة**. الأسهم المتقطعة تُظهر علاقة الأشجار الفرعية — عنصر Tree في الأب يحتوي على مفتاح الجذر لشجرة Merk الابن.

في قاعدة بيانات تقليدية، قد تُخزَّن البيانات في مخزن مفتاح-قيمة مسطح مع
شجرة Merkle (شجرة ميركل) واحدة في الأعلى للتوثيق. يتبع GroveDB نهجاً مختلفاً:
فهو يُعشّش أشجار ميركل داخل أشجار ميركل. هذا يمنحك:

1. **فهارس ثانوية فعّالة** — الاستعلام بأي مسار، وليس فقط بالمفتاح الأساسي
2. **براهين تشفيرية مدمجة** — إثبات وجود (أو غياب) أي بيانات
3. **بيانات تجميعية** — يمكن للأشجار تلقائياً جمع أو عدّ أو تجميع
   أبنائها بطرق أخرى
4. **عمليات ذرية عبر الأشجار** — العمليات الدفعية تمتد عبر أشجار فرعية متعددة

## لماذا وُجد GroveDB

صُمّم GroveDB لمنصة **Dash Platform**، وهي منصة تطبيقات لامركزية
حيث يجب أن تكون كل جزء من الحالة:

- **موثّقاً**: أي عقدة يمكنها إثبات أي جزء من الحالة لعميل خفيف
- **حتمياً**: كل عقدة تحسب نفس جذر الحالة بالضبط
- **فعّالاً**: يجب أن تكتمل العمليات ضمن قيود وقت الكتلة
- **قابلاً للاستعلام**: التطبيقات تحتاج استعلامات غنية، وليس فقط بحث بالمفتاح

النهج التقليدية تقصر عن ذلك:

| النهج | المشكلة |
|-------|---------|
| شجرة ميركل عادية | تدعم فقط البحث بالمفتاح، بدون استعلامات نطاق |
| Ethereum MPT | إعادة موازنة مكلفة، أحجام براهين كبيرة |
| مفتاح-قيمة مسطح + شجرة واحدة | بدون استعلامات هرمية، برهان واحد يغطي كل شيء |
| شجرة B-tree | غير مُمركَلة بطبيعتها، توثيق معقد |

يحل GroveDB هذه المشاكل بدمج **ضمانات التوازن المثبتة لأشجار AVL**
مع **التعشيش الهرمي** و**نظام أنواع عناصر غني**.

## نظرة عامة على البنية

GroveDB منظّم في طبقات مميزة، كل منها بمسؤولية واضحة:

```mermaid
graph TD
    APP["<b>Application Layer</b><br/>Dash Platform, etc.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB Core</b> — <code>grovedb/src/</code><br/>Hierarchical subtree management · Element type system<br/>Reference resolution · Batch ops · Multi-layer proofs"]

    MERK["<b>Merk Layer</b> — <code>merk/src/</code><br/>Merkle AVL tree · Self-balancing rotations<br/>Link system · Blake3 hashing · Proof encoding"]

    STORAGE["<b>Storage Layer</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 column families · Blake3 prefix isolation · Batched writes"]

    COST["<b>Cost Layer</b> — <code>costs/src/</code><br/>OperationCost tracking · CostContext monad<br/>Worst-case &amp; average-case estimation"]

    APP ==>|"writes ↓"| GROVE
    GROVE ==>|"tree ops"| MERK
    MERK ==>|"disk I/O"| STORAGE
    STORAGE -.->|"cost accumulation ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"reads ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

تتدفق البيانات **نزولاً** عبر هذه الطبقات أثناء الكتابة و**صعوداً** أثناء القراءة.
كل عملية تُراكم التكاليف أثناء عبورها للمكدس، مما يتيح محاسبة دقيقة
للموارد.

---
