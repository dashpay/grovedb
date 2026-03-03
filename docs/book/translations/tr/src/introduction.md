# Giris -- GroveDB Nedir?

## Temel Fikir

GroveDB bir **hiyerarsik kimlik dogrulanabilir veri yapisidir** -- ozunde Merkle AVL agaclari uzerine kurulmus bir *grove* (agaclar agaci). Veritabanindaki her dugum, kriptografik olarak kimlik dogrulanabilir bir agacin parcasidir ve her agac cocuk olarak baska agaclari icerebilir; boylece dogrulanabilir durumun (state) derin bir hiyerarsisi olusur.

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

> Her renkli kutu **ayri bir Merk agacidir**. Kesikli oklar alt agac (subtree) iliskisini gosterir -- ust agactaki bir Tree elementi, alt Merk'in kok anahtarini (root key) icerir.

Geleneksel bir veritabaninda, veriyi duz bir anahtar-deger deposunda saklayabilir ve kimlik dogrulama (authentication) icin ustune tek bir Merkle agaci koyabilirsiniz. GroveDB farkli bir yaklasim benimser: Merkle agaclarini Merkle agaclarinin icine yerlestirir. Bu size sunlari saglar:

1. **Verimli ikincil dizinler (secondary index)** -- sadece birincil anahtar degil, herhangi bir yol (path) uzerinden sorgulama
2. **Kompakt kriptografik ispatlar (proof)** -- herhangi bir verinin varligini (veya yoklugunu) kanitlama
3. **Toplam veri** -- agaclar cocuklarini otomatik olarak toplayabilir, sayabilir veya baska sekillerde birlestirebilir
4. **Atomik capraz agac islemleri** -- toplu islemler birden fazla alt agaci kapsayabilir

## GroveDB Neden Var?

GroveDB, her durum parcasinin asagidakileri karsilamasi gereken merkezi olmayan bir uygulama platformu olan **Dash Platform** icin tasarlandi:

- **Kimlik dogrulanabilir**: Herhangi bir dugum, herhangi bir durum parcasini hafif bir istemciye (light client) kanitlayabilir
- **Belirleyici (Deterministic)**: Her dugum tam olarak ayni durum kokunu (state root) hesaplar
- **Verimli**: Islemler blok suresi kisitlamalari icinde tamamlanmalidir
- **Sorgulanabilir**: Uygulamalar sadece anahtar aramalari degil, zengin sorgulara ihtiyac duyar

Geleneksel yaklasimlar yetersiz kalir:

| Yaklasim | Sorun |
|----------|-------|
| Duz Merkle Agaci | Yalnizca anahtar aramalarini destekler, aralik sorgulari yok |
| Ethereum MPT | Pahali yeniden dengeleme, buyuk ispat boyutlari |
| Duz anahtar-deger + tek agac | Hiyerarsik sorgu yok, tek bir ispat her seyi kapsar |
| B-agaci | Dogal olarak Merkle yapida degil, karmasik kimlik dogrulama |

GroveDB, **AVL agaclarinin kanitlanmis denge garantilerini**, **hiyerarsik icleme** ve **zengin element tip sistemiyle** birlestirerek bunlari cozer.

## Mimari Genel Bakis

GroveDB, her biri acik bir sorumluluga sahip farkli katmanlar halinde organize edilmistir:

```mermaid
graph TD
    APP["<b>Uygulama Katmani</b><br/>Dash Platform, vb.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB Cekirdegi</b> — <code>grovedb/src/</code><br/>Hiyerarsik alt agac yonetimi · Element tip sistemi<br/>Referans cozumlemesi · Toplu islemler · Cok katmanli ispatlar"]

    MERK["<b>Merk Katmani</b> — <code>merk/src/</code><br/>Merkle AVL agaci · Kendi kendini dengeleyen rotasyonlar<br/>Link sistemi · Blake3 hashleme · Ispat kodlamasi"]

    STORAGE["<b>Depolama Katmani</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 sutun ailesi · Blake3 onek izolasyonu · Toplu yazmalar"]

    COST["<b>Maliyet Katmani</b> — <code>costs/src/</code><br/>OperationCost takibi · CostContext monad<br/>En kotu durum &amp; ortalama durum tahmini"]

    APP ==>|"yazmalar ↓"| GROVE
    GROVE ==>|"agac islemleri"| MERK
    MERK ==>|"disk I/O"| STORAGE
    STORAGE -.->|"maliyet birikimi ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"okumalar ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Veri, yazmalar sirasinda bu katmanlar boyunca **asagi** akar ve okumalar sirasinda **yukari** akar. Her islem, yigin boyunca ilerledikce maliyetleri biriktirir ve kesin kaynak muhasebesi yapilmasini saglar.

---
