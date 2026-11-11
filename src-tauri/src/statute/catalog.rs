//! Laws shipped in full, plus a few aliases for laws fetched on demand.
//!
//! Official names that differ from the e-Gov list label are included as aliases.
//! The list label is also stored when the bundle is built.

#[derive(Clone, Copy)]
pub struct CatalogEntry {
    pub law_id: &'static str,
    /// Full text is packed into the app. `false` only adds the alias so a later
    /// fetch can resolve the name.
    pub bundle: bool,
    pub aliases: &'static [&'static str],
}

pub static CATALOG: &[CatalogEntry] = &[
    entry("321CONSTITUTION", true, &["日本国憲法", "憲法"]),
    entry("129AC0000000089", true, &["民法"]),
    entry("132AC0000000048", true, &["商法"]),
    entry("417AC0000000086", true, &["会社法"]),
    entry("140AC0000000045", true, &["刑法"]),
    entry("323AC0000000131", true, &["刑事訴訟法", "刑訴法"]),
    entry("408AC0000000109", true, &["民事訴訟法", "民訴法"]),
    entry("354AC0000000004", true, &["民事執行法"]),
    entry("401AC0000000091", true, &["民事保全法"]),
    entry("416AC0000000075", true, &["破産法"]),
    entry("411AC0000000225", true, &["民事再生法"]),
    entry("414AC0000000154", true, &["会社更生法"]),
    entry("403AC0000000090", true, &["借地借家法"]),
    entry("322AC0000000049", true, &["労働基準法", "労基法"]),
    entry("419AC0000000128", true, &["労働契約法"]),
    entry("324AC0000000174", true, &["労働組合法", "労組法"]),
    entry(
        "322AC0000000054",
        true,
        &[
            "私的独占の禁止及び公正取引の確保に関する法律",
            "独占禁止法",
            "独禁法",
        ],
    ),
    entry("405AC0000000047", true, &["不正競争防止法"]),
    entry("334AC0000000121", true, &["特許法"]),
    entry("334AC0000000123", true, &["実用新案法"]),
    entry("334AC0000000125", true, &["意匠法"]),
    entry("334AC0000000127", true, &["商標法"]),
    entry("345AC0000000048", true, &["著作権法"]),
    entry("337AC0000000139", true, &["行政事件訴訟法", "行訴法"]),
    entry("405AC0000000088", true, &["行政手続法"]),
    entry("426AC0000000068", true, &["行政不服審査法"]),
    entry("322AC0000000125", true, &["国家賠償法", "国賠法"]),
    entry("329AC0000000100", true, &["利息制限法"]),
    entry("412AC0000000061", true, &["消費者契約法"]),
    entry(
        "351AC0000000057",
        true,
        &["特定商取引に関する法律", "特定商取引法", "特商法"],
    ),
    entry(
        "415AC0000000057",
        true,
        &["個人情報の保護に関する法律", "個人情報保護法"],
    ),
    entry("416AC0000000123", true, &["不動産登記法"]),
    entry(
        "337AC0000000069",
        true,
        &["建物の区分所有等に関する法律", "区分所有法"],
    ),
    entry("406AC0000000085", true, &["製造物責任法"]),
    entry("420AC0000000056", true, &["保険法"]),
    entry("307AC0000000020", true, &["手形法"]),
    entry("308AC0000000057", true, &["小切手法"]),
    entry("418AC0000000108", true, &["信託法"]),
    entry(
        "418AC0000000048",
        true,
        &["一般社団法人及び一般財団法人に関する法律", "一般社団法人法"],
    ),
    entry("423AC0000000052", true, &["家事事件手続法"]),
    entry("415AC0000000109", true, &["人事訴訟法"]),
    entry(
        "331AC0000000120",
        true,
        &[
            "製造委託等に係る中小受託事業者に対する代金の支払の遅延等の防止に関する法律",
            "下請代金支払遅延等防止法",
            "下請法",
            "取適法",
        ],
    ),
    entry("323AC0000000168", true, &["少年法"]),
    entry("324AC1000000205", true, &["弁護士法"]),
    entry("326AC1000000222", true, &["民事調停法"]),
    entry("322AC0000000224", true, &["戸籍法"]),
    entry(
        "411AC0000000136",
        true,
        &[
            "組織的な犯罪の処罰及び犯罪収益の規制等に関する法律",
            "組織的犯罪処罰法",
        ],
    ),
    entry("415AC0000000138", true, &["仲裁法"]),
    entry(
        "413AC0000000137",
        true,
        &[
            "特定電気通信による情報の流通によって発生する権利侵害等への対処に関する法律",
            "プロバイダ責任制限法",
            "情報流通プラットフォーム対処法",
        ],
    ),
    entry(
        "322AC0000000050",
        true,
        &["労働者災害補償保険法", "労災保険法"],
    ),
    entry(
        "347AC0000000113",
        true,
        &[
            "雇用の分野における男女の均等な機会及び待遇の確保等に関する法律",
            "男女雇用機会均等法",
            "均等法",
        ],
    ),
    entry(
        "403AC0000000076",
        true,
        &[
            "育児休業、介護休業等育児又は家族介護を行う労働者の福祉に関する法律",
            "育児介護休業法",
        ],
    ),
    // Full text is large. The name still resolves so a missing article can be fetched.
    entry("323AC0000000025", false, &["金融商品取引法", "金商法"]),
];

const fn entry(
    law_id: &'static str,
    bundle: bool,
    aliases: &'static [&'static str],
) -> CatalogEntry {
    CatalogEntry {
        law_id,
        bundle,
        aliases,
    }
}

pub fn aliases_for(law_id: &str) -> &'static [&'static str] {
    CATALOG
        .iter()
        .find(|e| e.law_id == law_id)
        .map(|e| e.aliases)
        .unwrap_or(&[])
}
