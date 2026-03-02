# Kuantum Kriptografi -- Kuantum Sonrasi Tehdit Analizi

Bu bolum, kuantum bilgisayarlarin GroveDB'de kullanilan kriptografik temel bilesenleri ve bunun uzerine insa edilen korunmali islem protokollerini (Orchard, Dash Platform) nasil etkileyecegini analiz eder. Hangi bilesenlerin savunmasiz, hangilerinin guvenli oldugunu, "simdi topla, sonra coz" kavramininin depolanan veriler icin ne anlama geldigini ve hibrit KEM tasarimlari dahil hangi azaltma stratejilerinin mevcut oldugunu kapsar.

## Onemli Iki Kuantum Algoritmasi

Pratikte kriptografi ile ilgili yalnizca iki kuantum algoritmasi vardir:

**Shor algoritmasi** ayrik logaritma problemini (ve tamsayi carpanlarini ayirmayi) polinom zamanda cozer. Pallas gibi 255 bitlik bir eliptik egri icin yaklasik 510 mantiksal kubit gerektirir -- ancak hata duzeltme yuku ile gercek gereksinim yaklasik 4 milyon fiziksel kubittir. Shor algoritmasi, anahtar boyutundan bagimsiz olarak tum eliptik egri kriptografisini **tamamen kirar**.

**Grover algoritmasi** kaba kuvvet aramasi icin ikinci dereceden hizlanma saglar. 256 bitlik bir simetrik anahtar etkin olarak 128 bit olur. Ancak, 128 bitlik bir anahtar uzayi icin Grover'in devre derinligi hala 2^64 kuantum islemidir -- bircok kriptograf, dekoherans sinirlari nedeniyle bunun gercek donanim uzerinde asla pratik olmayacagina inanmaktadir. Grover guvenlik marjlarini azaltir ancak iyi parametrelenmis simetrik kriptografiyi kirmaz.

| Algoritma | Hedefler | Hizlanma | Pratik etki |
|-----------|---------|---------|------------------|
| **Shor** | ECC ayrik logaritma, RSA carpanlarina ayirma | Ustel (polinom zaman) | ECC'nin **tamamen kirilmasi** |
| **Grover** | Simetrik anahtar arama, hash on-goruntu | Ikinci dereceden (anahtar bitlerini yariya indirir) | 256 bit -> 128 bit (hala guvenli) |

## GroveDB'nin Kriptografik Temel Bilesenleri

GroveDB ve Orchard tabanli korumali protokol, eliptik egri ile simetrik/hash tabanli temel bilesenlerin bir karisimini kullanir. Asagidaki tablo, her bir temel bileseni kuantum savunmasizligina gore siniflandirir:

### Kuantuma Karsi Savunmasiz (Shor algoritmasi -- kuantum sonrasi 0 bit)

| Temel bilesen | Kullanim yeri | Ne kirilir |
|-----------|-----------|-------------|
| **Pallas ECDLP** | Not taahhutleri (cmx), gecici anahtarlar (epk/esk), goruntuuleme anahtarlari (ivk), odeme anahtarlari (pk_d), nullifier turetimi | Herhangi bir ozel anahtari kamusal karsiliginden kurtar |
| **ECDH anahtar anlasma** (Pallas) | Not sifreli metinleri icin simetrik sifreleme anahtari turetimi | Paylasilan sirri kurtar -> tum notlari coz |
| **Sinsemilla hash** | CommitmentTree Merkle yollari, devre ici hashleme | Carpisma direnci ECDLP'ye baglidir; Pallas kirildiginda zayiflar |
| **Halo 2 IPA** | ZK ispat sistemi (Pasta egrileri uzerinde polinom taahhut) | Yanlis ifadeler icin ispat sahteciligi (sahtecilik, yetkisiz harcama) |
| **Pedersen taahhutleri** | Islem tutarlarini gizleyen deger taahhutleri (cv_net) | Gizli tutarlari kurtar; bakiye ispatlarini sahtelestir |

### Kuantuma Karsi Guvenli (Grover algoritmasi -- kuantum sonrasi 128+ bit)

| Temel bilesen | Kullanim yeri | Kuantum sonrasi guvenlik |
|-----------|-----------|----------------------|
| **Blake3** | Merk agaci dugum hashleri, MMR dugumleri, BulkAppendTree durum kokleri, alt agac yol onekleri | 128 bit on-goruntu, 128 bit ikinci on-goruntu |
| **BLAKE2b-256** | Simetrik anahtar turetimi icin KDF, giden sifre anahtari, PRF^expand | 128 bit on-goruntu |
| **ChaCha20-Poly1305** | enc_ciphertext ve out_ciphertext sifreleme (256 bit anahtarlar) | 128 bit anahtar arama (guvenli, ancak ECDH uzerinden anahtar turetim yolu guvenli degil) |
| **PRF^expand** (BLAKE2b-512) | rseed'den esk, rcm, psi turetimi | 128 bit PRF guvenligi |

### GroveDB Altyapisi: Tamamen Kuantuma Karsi Guvenli

GroveDB'nin kendi veri yapilari yalnizca Blake3 hashlemesine dayanir:

- **Merk AVL agaclari** -- dugum hashleri, combined_value_hash, cocuk hash yayilimi
- **MMR agaclari** -- ic dugum hashleri, tepe hesabi, kok turetimi
- **BulkAppendTree** -- tampon hash zincirleri, yogun Merkle kokleri, donem MMR
- **CommitmentTree durum koku** -- `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Alt agac yol onekleri** -- yol segmentlerinin Blake3 hashlemesi
- **V1 ispatlar** -- Merk hiyerarsisi uzerinden dogrulama zincirleri

**Degisiklik gerekmez.** GroveDB'nin Merk agaci ispatlar, MMR tutarlilik kontrolleri, BulkAppendTree donem kokleri ve tum V1 ispat dogrulama zincirleri kuantum bilgisayarlara karsi guvenli kalir. Hash tabanli altyapi, sistemin kuantum sonrasi en guclu parçasidir.

## Geriye Donuk ve Gercek Zamanli Tehditler

Bu ayrım, neyin ne zaman duzeltilmesi gerektigini onceliklendirmek icin kritiktir.

**Geriye donuk tehditler** halihazirda depolanan verileri tehlikeye atar. Bir saldirgan bugun verileri kaydeder ve kuantum bilgisayarlar kullanilabilir hale geldiginde cozer. Bu tehditler **sonradan azaltilamaz** -- veri zincir uzerinde oldugunda yeniden sifrelenemez veya geri alinamaz.

**Gercek zamanli tehditler** yalnizca gelecekte olusturulan islemleri etkiler. Kuantum bilgisayara sahip bir saldirgan imza veya ispat sahteciligi yapabilir, ancak yalnizca yeni islemler icin. Eski islemler ag tarafindan zaten dogrulanmis ve onaylanmistir.

| Tehdit | Tur | Aciga cikan | Aciliyet |
|--------|------|---------------|---------|
| **Not cozme** (enc_ciphertext) | **Geriye donuk** | Not icerikleri: alici, tutar, not, rseed | **Yuksek** -- kalici depolama |
| **Deger taahhut acma** (cv_net) | **Geriye donuk** | Islem tutarlari (ancak gonderen/alici degil) | **Orta** -- yalnizca tutarlar |
| **Gonderen kurtarma verileri** (out_ciphertext) | **Geriye donuk** | Gonderenin gonderilen notlar icin kurtarma anahtarlari | **Yuksek** -- kalici depolama |
| Harcama yetkilendirme sahteciligi | Gercek zamanli | Yeni harcama imzalari sahtelestirebilir | Dusuk -- QC gelmeden once yukselt |
| Halo 2 ispat sahteciligi | Gercek zamanli | Yeni ispatlar sahtelestirebilir (sahtecilik) | Dusuk -- QC gelmeden once yukselt |
| Sinsemilla carpismasi | Gercek zamanli | Yeni Merkle yollari sahtelestirebilir | Dusuk -- ispat sahteciligi kapsaminda |
| Baglama imza sahteciligi | Gercek zamanli | Yeni bakiye ispatları sahtelestirebilir | Dusuk -- QC gelmeden once yukselt |

### Tam Olarak Ne Aciga Cikar?

**Not sifreleme kirilirsa** (birincil HNDL tehdidi):

Kuantum saldirgan, Shor algoritmasi araciligiyla depolanan `epk`'dan `esk`'yi kurtarir, ECDH paylasilan sirri hesaplar, simetrik anahtari tureterek `enc_ciphertext`'i cozer. Bu, tam not duz metnini ortaya cikarir:

| Alan | Boyut | Ne ortaya cikarir |
|-------|------|----------------|
| version | 1 byte | Protokol surumu (hassas degil) |
| diversifier | 11 bytes | Alicinin adres bileseni |
| value | 8 bytes | Kesin islem tutari |
| rseed | 32 bytes | Nullifier baglantisini etkinlestirir (islem grafi anonimligini kaldirir) |
| memo | 36 bytes (DashMemo) | Uygulama verileri, potansiyel olarak tanimlayici |

`rseed` ve `rho` (sifre metni ile birlikte depolanan) ile saldirgan `esk = PRF(rseed, rho)` hesaplayabilir ve gecici anahtar baglamasini dogrulayabilir. Diversifier ile birlestirildiginde, tum islem gecmisi boyunca girisleri ciktilara baglar -- **korumali havuzun tam anonimlik kaybı**.

**Yalnizca deger taahhutleri kirilirsa** (ikincil HNDL tehdidi):

Saldirgan, ECDLP'yi cozerek `cv_net = [v]*V + [rcv]*R`'den `v`'yi kurtarir. Bu, **islem tutarlarini ortaya cikarir ancak gonderen veya alici kimliklerini degil**. Saldirgan "birisi birisine 5.0 Dash gonderdi" gorur ancak not sifrelemeyi de kirmadan tutari herhangi bir adrese veya kimlige baglayamaz.

Tek basina, baglanti olmadan tutarlar sinirli kullanima sahiptir. Ancak dis verilerle (zamanlama, bilinen faturalar, kamusal taleplerle eslesen tutarlar) birlestirildiginde korelasyon saldirilari mumkun hale gelir.

## "Simdi Topla, Sonra Coz" Saldirisi

Bu, en acil ve pratik kuantum tehdididir.

**Saldiri modeli:** Devlet duzeyinde bir saldirgan (veya yeterli depolama alani olan herhangi bir taraf) bugun zincir uzerindeki tum korumali islem verilerini kaydeder. Bu veriler blokzincirde herkese acik ve degistirilemezdir. Saldirgan kriptografik olarak ilgili bir kuantum bilgisayar (CRQC) bekler, ardindan:

```text
Step 1: Read stored record from CommitmentTree BulkAppendTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Step 2: Solve ECDLP on Pallas via Shor's algorithm:
        epk = [esk] * g_d  →  recover esk

Step 3: Compute shared secret:
        shared_secret = [esk] * pk_d

Step 4: Derive symmetric key (BLAKE2b is quantum-safe, but input is compromised):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Step 5: Decrypt enc_ciphertext using ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Step 6: With rseed + rho, link nullifiers to note commitments:
        esk = PRF(rseed, rho)
        → full transaction graph reconstruction
```

**Temel icerik:** Simetrik sifreleme (ChaCha20-Poly1305) tamamen kuantuma karsi guvenlidir. Savunmasizlik tamamen **anahtar turetim yolunda** -- simetrik anahtar ECDH paylasilan sirrindan turetilir ve ECDH Shor algoritmasi tarafindan kirilir. Saldirgan sifrelemeyi kirmaz; anahtari kurtarir.

**Geriye donukluk:** Bu saldiri **tamamen geriye donuktur**. Bir CRQC mevcut oldugunda zincir uzerinde depolanan her sifrelenmis not cozulebilir. Veriler sonradan yeniden sifrelenemez veya korunamaz. Bu nedenle veriler depolanmadan once, sonra degil, ele alinmalidir.

## Azaltma: Hibrit KEM (ML-KEM + ECDH)

HNDL'ye karsi savunma, simetrik sifreleme anahtarini **iki bagimsiz anahtar anlasma mekanizmasindan** turetmektir, boylece yalnizca birini kirmak yetersiz kalir. Buna hibrit KEM denir.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM, Modul Hatali Ogrenme (MLWE) problemine dayanan NIST standartlastirilmis (FIPS 203, Agustos 2024) kuantum sonrasi anahtar kapsulleme mekanizmasidir.

| Parametre | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Acik anahtar (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Sifre metni (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Paylasilan sir | 32 bytes | 32 bytes | 32 bytes |
| NIST Kategorisi | 1 (128 bit) | **3 (192 bit)** | 5 (256 bit) |

**ML-KEM-768** onerilen secimdir -- X-Wing, Signal'in PQXDH'si ve Chrome/Firefox TLS hibrit anahtar degisiminde kullanilan parametre setidir. Kategori 3, gelecekteki kafes kriptanaliz gelismelerine karsi rahat bir marj saglar.

### Hibrit Semanin Calismasi

**Mevcut akis (savunmasiz):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Hibrit akis (kuantuma direncli):**

```text
Sender:
  esk = PRF(rseed, rho)                    // unchanged
  epk = [esk] * g_d                         // unchanged
  ss_ecdh = [esk] * pk_d                    // same ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NEW: lattice-based KEM
                                                // ek_pq from recipient's address

  K_enc = BLAKE2b(                          // MODIFIED: combines both secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // unchanged
```

**Alici sifre cozme:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Guvenlik Garantisi

Birlesik KEM, **herhangi bir** bilesen KEM guvenliyse IND-CCA2 guvenlidir. Bu, PRF (BLAKE2b uygun) kullanan KEM birlestiricileri icin [Giacon, Heuer ve Poettering (2018)](https://eprint.iacr.org/2018/024) tarafindan resmi olarak kanitlanmistir ve [X-Wing guvenlik kaniti](https://eprint.iacr.org/2024/039) tarafindan bagimsiz olarak kanitlanmistir.

| Senaryo | ECDH | ML-KEM | Birlesik anahtar | Durum |
|----------|------|--------|-------------|--------|
| Klasik dunya | Guvenli | Guvenli | **Guvenli** | Ikisi de sagam |
| Kuantum ECC'yi kirar | **Kirilmis** | Guvenli | **Guvenli** | ML-KEM korur |
| Kafes gelismeleri ML-KEM'i kirar | Guvenli | **Kirilmis** | **Guvenli** | ECDH korur (bugunku gibi) |
| Ikisi de kirilmis | Kirilmis | Kirilmis | **Kirilmis** | Iki es zamanli atilim gerektirir |

### Boyut Etkisi

Hibrit KEM, depolanan her nota ML-KEM-768 sifre metnini (1,088 bytes) ekler ve gonderenin kurtarma icin ML-KEM paylasilan sirri icermesi amaciyla giden sifre metnini genisletir:

**Not basina depolanan kayit:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Current (280 bytes)         Hybrid (1,400 bytes)               │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NEW       │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Olcekte depolama:**

| Not sayisi | Mevcut (280 B) | Hibrit (1,400 B) | Fark |
|-------|----------------|------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Adres boyutu:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

1,184 byte ML-KEM acik anahtari, gonderenlerin kapsulleme yapabilmesi icin adrese dahil edilmelidir. Yaklasik 1,960 Bech32m karakter ile buyuktur ancak bir QR koduna (maksimum ~2,953 alfanumerik karakter) hala sigar.

### Anahtar Yonetimi

ML-KEM anahtar cifti, harcama anahtarindan deterministik olarak turetilir:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (all existing Orchard key derivation unchanged)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (public, included in address)
              dk_pq: 2,400 bytes (private, part of viewing key)
```

**Yedekleme degisikligi gerekmez.** Mevcut 24 kelimelik tohum cumlesi ML-KEM anahtarini kapsar cunku harcama anahtarindan deterministik olarak turetilir. Cuzdan kurtarma eskisi gibi calisir.

**Cesitlendirilmis adresler** hepsi ayni `ek_pq`'yu paylasiyor cunku ML-KEM'de Pallas skaler carpimi gibi dogal bir cesitlendirme mekanizmasi yok. Bu, bir kullanicinin iki adresine sahip bir gozlemcinin `ek_pq`'yu karsilastirarak bunlari baglayabilecegi anlamina gelir.

### Deneme Sifre Cozme Performansi

| Adim | Mevcut | Hibrit | Fark |
|------|---------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | -- |
| ML-KEM-768 Decaps | -- | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | -- |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | -- |
| **Not basina toplam** | **~101 us** | **~141 us** | **+%40 ek yuk** |

100,000 not tarama: ~10.1 sn -> ~14.1 sn. Ek yuk anlamlidir ancak engelleyici degildir. ML-KEM kapsulden cikarma, (eliptik egri islemlerinin aksine) yiginlama avantaji olmadan sabit zamanlidir, bu nedenle dogrusal olarak olceklenir.

### ZK Devreleri Uzerindeki Etkisi

**Yok.** Hibrit KEM tamamen tasima/sifreleme katmanindadir. Halo 2 devresi not varligini, nullifier dogrulugunu ve deger dengesini kanitlar -- sifreleme hakkinda hicbir sey kanitlamaz. Ispat anahtarlari, dogrulama anahtarlari veya devre kisitlamalarinda degisiklik yoktur.

### Sektorle Karsilastirma

| Sistem | Yaklasim | Durum |
|--------|----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, tum kullanicilar icin zorunlu | **Yayinlanmis** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 hibrit anahtar degisimi | **Yayinlanmis** (2024) |
| **X-Wing** (IETF taslagi) | X25519 + ML-KEM-768, amaca yonelik birlestirici | Taslak standart |
| **Zcash** | Kuantum kurtarilabilirlik taslak ZIP (fon kurtarma, sifreleme degil) | Yalnizca tartisma |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (onerilmis) | Tasarim asamasi |

## Ne Zaman Yayinlanmali

### Zaman Cizelgesi Sorusu

- **Mevcut durum (2026):** Hicbir kuantum bilgisayar 255 bit ECC'yi kiramaz. Gosterilmis en buyuk kuantum carpanlarina ayirma: ~50 bit. Fark: buyukluk siralari.
- **Yakin vadeli (2030-2035):** IBM, Google, Quantinuum'dan donanim yol haritalari milyonlarca kubiti hedefliyor. ML-KEM uygulamalari ve parametre setleri olgunlasmis olacak.
- **Orta vadeli (2035-2050):** Cogu tahmin CRQC varisini bu pencereye koyar. Bugun toplanan HNDL verileri risk altindadir.
- **Uzun vadeli (2050+):** Kriptograflar arasindaki uzlasma: buyuk olcekli kuantum bilgisayarlar "eger" degil "ne zaman" meselesidir.

### Onerilen Strateji

**1. Simdi yukseltme icin tasarlayin.** Depolanan kayit formati, `TransmittedNoteCiphertext` yapisi ve BulkAppendTree giris duzeni surumlu ve genisletilebilir olsun. Dusuk maliyetlidir ve daha sonra hibrit KEM ekleme secenegini korur.

**2. Hazir oldugunda hibrit KEM'i yayinlayin, zorunlu kılın.** Iki havuz (klasik ve hibrit) sunmayin. Anonimlik setini bolmek korumali islemlerin amacini bozar -- daha kucuk bir grupta saklanan kullanicilar daha az ozeldir, daha fazla degil. Yayinlandiginda her not hibrit semayi kullanir.

**3. 2028-2030 penceresini hedefleyin.** Bu, herhangi bir gercekci kuantum tehditten cok once ancak ML-KEM uygulamalari ve parametre boyutlari stabilize olduktan sonradir. Ayrica Zcash ve Signal'in yayinlama deneyiminden ogrenmeye izin verir.

**4. Tetikleyici olaylari izleyin:**
- NIST veya NSA'nin kuantum sonrasi goc son tarihleri dayatmasi
- Kuantum donanımında onemli ilerlemeler (hata duzeltme ile >100,000 fiziksel kubit)
- Kafes problemlerine karsi kriptanalitik ilerlemeler (ML-KEM secimini etkiler)

### Acil Eylem Gerektirmeyen Sey

| Bilesen | Neden bekleyebilir |
|-----------|----------------|
| Harcama yetkilendirme imzalari | Sahtecilik gercek zamanlidir, geriye donuk degildir. CRQC gelmeden ML-DSA/SLH-DSA'ya yukselt. |
| Halo 2 ispat sistemi | Ispat sahteciligi gercek zamanlidir. Gerektiginde STARK tabanli sisteme gec. |
| Sinsemilla carpisma direnci | Yalnizca yeni saldirilar icin yararli, geriye donuk degil. Ispat sistemi gocu kapsaminda. |
| GroveDB Merk/MMR/Blake3 altyapisi | **Mevcut kriptografik varsayimlar altinda zaten kuantum guvenli.** Bilinen saldirilara dayali olarak herhangi bir eylem gerekli degildir. |

## Kuantum Sonrasi Alternatifler Referansi

### Sifreleme Icin (ECDH yerine)

| Sema | Tur | Acik anahtar | Sifre metni | NIST Kategorisi | Notlar |
|--------|------|-----------|-----------|---------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192 bit) | FIPS 203, sektor standardi |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128 bit) | Daha kucuk, dusuk marj |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256 bit) | Hibrit icin asiri |

### Imzalar Icin (RedPallas/Schnorr yerine)

| Sema | Tur | Acik anahtar | Imza | NIST Kategorisi | Notlar |
|--------|------|-----------|----------|---------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, hizli |
| SLH-DSA (SPHINCS+) | Hash-based | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, muhafazakar |
| XMSS/LMS | Hash-based (stateful) | 60 B | 2,500 B | varies | Durumlu -- yeniden kullanım = kirilma |

### ZK Ispatlar Icin (Halo 2 yerine)

| Sistem | Varsayim | Ispat boyutu | Kuantum sonrasi | Notlar |
|--------|-----------|-----------|-------------|-------|
| STARKs | Hash fonksiyonlari (carpisma direnci) | ~100-400 KB | **Yes** | StarkNet tarafindan kullanilir |
| Plonky3 | FRI (hash tabanli polinom taahhut) | ~50-200 KB | **Yes** | Aktif gelistirme |
| Halo 2 (mevcut) | Pasta egrileri uzerinde ECDLP | ~5 KB | **No** | Mevcut Orchard sistemi |
| Lattice SNARKs | MLWE | Arastirma | **Yes** | Uretime hazir degil |

### Rust Crate Ekosistemi

| Crate | Kaynak | FIPS 203 | Dogrulanmis | Notlar |
|-------|--------|----------|----------|-------|
| `libcrux-ml-kem` | Cryspen | Yes | Resmi olarak dogrulanmis (hax/F*) | En yuksek guvence |
| `ml-kem` | RustCrypto | Yes | Sabit zamanli, denetlenmemis | Ekosistem uyumlulugu |
| `fips203` | integritychain | Yes | Sabit zamanli | Saf Rust, no_std |

## Ozet

```text
┌─────────────────────────────────────────────────────────────────────┐
│  GROVEDB + ORCHARD ICIN KUANTUM TEHDIT OZETI                       │
│                                                                     │
│  MEVCUT VARSAYIMLAR ALTINDA GUVENLI (hash tabanli):                 │
│    ✓ Blake3 Merk agaclari, MMR, BulkAppendTree                     │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ ChaCha20-Poly1305 simetrik sifreleme                          │
│    ✓ Tum GroveDB kanit dogrulama zincirleri                         │
│                                                                     │
│  VERI DEPOLANMADAN ONCE DUZELT (geriye donuk HNDL):                │
│    ✗ Not sifreleme (ECDH anahtar anlasmasi) → Hibrit KEM           │
│    ✗ Deger taahhutleri (Pedersen) → tutarlar aciga cikar            │
│                                                                     │
│  KUANTUM BILGISAYARLAR GELMEDEN ONCE DUZELT                         │
│  (yalnizca gercek zamanli):                                         │
│    ~ Harcama yetkilendirmesi → ML-DSA / SLH-DSA                    │
│    ~ ZK kanitlari → STARKs / Plonky3                               │
│    ~ Sinsemilla → hash tabanli Merkle agaci                         │
│                                                                     │
│  ONERILEN ZAMAN CIZELGESI:                                          │
│    2026-2028: Yukseltilebilirlik icin tasarla, formatlari versionla │
│    2028-2030: Not sifreleme icin zorunlu hibrit KEM yayinla         │
│    2035+: Gerekirse imza ve ispat sistemini tasi                    │
└─────────────────────────────────────────────────────────────────────┘
```

---
