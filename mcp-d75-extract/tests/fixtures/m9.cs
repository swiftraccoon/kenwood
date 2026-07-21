public class m9
{
    public enum a : byte
    {
        a,
        b = 4,
    }

    private byte[] m_a0;

    public a BeatShift
    {
        get { return a.a; }
    }

    public bool TxInhibit
    {
        get { return false; }
    }

    public bool LedControl_Receive
    {
        get { return false; }
    }

    public string PowerOnMessage
    {
        get { return string.Empty; }
    }

    public byte[] PoweronBitmap
    {
        get { return m_a0; }
    }

    public void a0(m6 A_0)
    {
        A_0.a((byte)BeatShift, 4096);
        A_0.a(TxInhibit, 4097);
        A_0.a((byte)0, 4136);
        A_0.a(Convert.ToByte(LedControl_Receive), 0, 4136);
        A_0.c(PowerOnMessage, 4288, nb.c);
        A_0.a(PoweronBitmap, 327680);
        A_0.a(327680, 86400);
        this.child.b(A_0, 0);
    }
}
